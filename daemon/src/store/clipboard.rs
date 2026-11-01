//! The clipboard history: what was copied, newest first, and what happens to it.
//!
//! History is off until the user turns it on. It is shared by the D-Bus dispatch thread (property
//! reads, `ClipboardPush` and friends) and the search thread (the clipboard provider), so it
//! synchronises internally.
//!
//! **Persistence** (ticket 11) is behind [`HistoryStore`], so this module never touches a
//! keyring or a file itself: it loads through the seam on construction and writes the whole
//! history back after every change that alters it. Tests hand it an in-memory double; the daemon
//! hands it the encrypted file. When there is no store — no keyring, no data directory, an
//! unwritable file — the store degrades to memory and reports why, because losing history is a
//! nuisance and crashing the daemon is not an option (contract §9).
//!
//! **Secrets** are suppressed before they are stored, by [`crate::store::redact`]: a password
//! never reaches memory, let alone the disk.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::store::persist::{History, HistoryStore, PersistError};
use crate::store::redact;

/// One recorded copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipEntry {
    /// `clip:<digest>`, from contract §3's id table. Derived from the text alone, so re-copying the
    /// same text de-duplicates instead of adding a second entry.
    pub id: String,
    /// The copied text, exactly as it arrived. This is what the user gets back on a copy.
    pub text: String,
    /// Milliseconds since the Unix epoch, as the shell saw the copy.
    pub timestamp_ms: i64,
}

impl ClipEntry {
    pub fn new(text: impl Into<String>, timestamp_ms: i64) -> Self {
        let text = text.into();
        Self {
            id: entry_id(&text),
            text,
            timestamp_ms,
        }
    }
}

/// The stable id of a clipboard text: the `clip:` prefix plus a 64-bit FNV-1a digest of the text.
///
/// A digest rather than a counter so that the same text always has the same id — across a restart
/// (ticket 11) and across both halves of the daemon — which is what makes de-duplication and
/// `ClipboardDelete` work on values the user has seen.
///
/// The digest is **not** a secret: it identifies text the user copied, and the text itself is
/// encrypted at rest (see [`crate::store::persist`]).
pub fn entry_id(text: &str) -> String {
    format!("clip:{:016x}", fnv1a_64(text.as_bytes()))
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Why a copy was or was not recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// Recorded as the newest entry.
    Recorded,
    /// History is off: nothing is recorded.
    Disabled,
    /// There is no entry in an empty copy.
    Empty,
    /// The text looks like a secret and was dropped on purpose (ticket 11).
    Suppressed,
}

impl PushOutcome {
    pub fn is_recorded(self) -> bool {
        matches!(self, Self::Recorded)
    }
}

#[derive(Debug)]
struct Inner {
    enabled: bool,
    cap: usize,
    /// Newest first.
    entries: Vec<ClipEntry>,
    /// Drop text that looks like a secret before it is stored. On by default.
    suppress_secrets: bool,
}

/// Clipboard history: in memory always, and on disk when a [`HistoryStore`] is available.
#[derive(Debug)]
pub struct ClipboardStore {
    inner: Mutex<Inner>,
    /// Where the history is written. `None` means memory only.
    store: Option<Arc<dyn HistoryStore>>,
    /// How the history is being kept, fixed when the store was opened.
    storage: Storage,
    /// The most recent failure to read or write the history, if any. Diagnostics only: a failed
    /// write never loses what is in memory and never takes the daemon down.
    error: Mutex<Option<String>>,
}

/// How the history is being kept. Reported so that "empty" can be told from "not being kept".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storage {
    /// Encrypted entries at this path, surviving a restart.
    OnDisk { path: PathBuf },
    /// Nothing is written; `reason` says why.
    InMemory { reason: String },
}

impl Storage {
    /// Whether entries survive this process.
    pub fn is_persistent(&self) -> bool {
        matches!(self, Self::OnDisk { .. })
    }

    /// A phrase fit for a log line.
    pub fn describe(&self) -> String {
        match self {
            Self::OnDisk { path } => {
                format!("clipboard history is encrypted at {}", path.display())
            }
            Self::InMemory { reason } => {
                format!("clipboard history is kept in memory only: {reason}")
            }
        }
    }
}

impl Default for ClipboardStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardStore {
    /// How many entries are kept. Older copies fall off the end; the store is a convenience, not an
    /// archive. The result list is capped again for display by the extension.
    pub const DEFAULT_CAP: usize = 100;

    /// A fresh, memory-only store: history off, nothing recorded, nothing written.
    pub fn new() -> Self {
        Self::with_cap(Self::DEFAULT_CAP)
    }

    pub fn with_cap(cap: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                enabled: false,
                cap,
                entries: Vec::new(),
                suppress_secrets: true,
            }),
            store: None,
            storage: Storage::InMemory {
                reason: "no store is configured".to_string(),
            },
            error: Mutex::new(None),
        }
    }

    /// Open a store backed by `store`: the history is read now and written back after every change
    /// that alters it.
    ///
    /// A history that cannot be read degrades to memory rather than starting empty-and-writable —
    /// overwriting a file we could not decrypt would destroy history the user could still recover
    /// with the right key.
    pub fn open(store: Arc<dyn HistoryStore>) -> Self {
        let storage = store.storage();
        match store.load() {
            Ok(loaded) => Self {
                inner: Mutex::new(Inner {
                    enabled: loaded.enabled,
                    cap: Self::DEFAULT_CAP,
                    entries: loaded.entries,
                    suppress_secrets: true,
                }),
                store: Some(store),
                storage,
                error: Mutex::new(None),
            },
            Err(error) => Self::degraded(format!("{}: {}", store.describe(), error)),
        }
    }

    /// A memory-only store that says why it is not persistent.
    pub fn degraded(reason: impl Into<String>) -> Self {
        let mut store = Self::with_cap(Self::DEFAULT_CAP);
        store.storage = Storage::InMemory {
            reason: reason.into(),
        };
        store
    }

    /// Whether the history is being written down.
    pub fn storage(&self) -> Storage {
        self.storage.clone()
    }

    /// The last failure to read or write the history.
    pub fn error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Whether copies are being recorded.
    pub fn is_enabled(&self) -> bool {
        self.lock().enabled
    }

    /// Whether text that looks like a secret is dropped. On by default.
    pub fn suppresses_secrets(&self) -> bool {
        self.lock().suppress_secrets
    }

    /// Turn secret suppression on or off. Off means a password can land in history.
    pub fn set_suppress_secrets(&self, suppress: bool) {
        self.lock().suppress_secrets = suppress;
    }

    /// Turn recording on or off.
    ///
    /// Turning history off stops new entries and hides the list (contract §9) but keeps what was
    /// recorded: it is what the user asked to see again next time. `ClipboardClear` is the way to
    /// destroy it. The new state is written down (ticket 11), so a restart does not silently
    /// re-enable recording.
    pub fn set_enabled(&self, enabled: bool) {
        let mut inner = self.lock();
        if inner.enabled == enabled {
            return;
        }
        inner.enabled = enabled;
        self.persist(&inner);
    }

    /// Record `text` as the newest entry, reporting why if it was not.
    ///
    /// Ignored while history is off, for empty text, and — unless suppression is off — for text
    /// that looks like a secret. Text already in the history moves to the front with its new
    /// timestamp instead of appearing twice.
    pub fn push_with_outcome(&self, text: &str, timestamp_ms: i64) -> PushOutcome {
        let mut inner = self.lock();
        if !inner.enabled {
            return PushOutcome::Disabled;
        }
        if text.is_empty() {
            return PushOutcome::Empty;
        }
        if inner.suppress_secrets && redact::is_secret(text) {
            return PushOutcome::Suppressed;
        }

        let entry = ClipEntry::new(text, timestamp_ms);
        inner.entries.retain(|existing| existing.id != entry.id);
        inner.entries.insert(0, entry);
        let cap = inner.cap;
        inner.entries.truncate(cap);
        self.persist(&inner);
        PushOutcome::Recorded
    }

    /// Record `text` as the newest entry. `false` when nothing was recorded — while history is off,
    /// for empty text, and for a suppressed secret.
    pub fn push(&self, text: &str, timestamp_ms: i64) -> bool {
        self.push_with_outcome(text, timestamp_ms).is_recorded()
    }

    /// The entries, newest first. Empty while history is off (contract §9).
    pub fn list(&self) -> Vec<ClipEntry> {
        let inner = self.lock();
        if !inner.enabled {
            return Vec::new();
        }
        inner.entries.clone()
    }

    /// Drop one entry. `false` when no entry has that id.
    pub fn delete(&self, id: &str) -> bool {
        let mut inner = self.lock();
        let before = inner.entries.len();
        inner.entries.retain(|entry| entry.id != id);
        if inner.entries.len() == before {
            return false;
        }
        self.persist(&inner);
        true
    }

    /// Drop every entry.
    pub fn clear(&self) {
        let mut inner = self.lock();
        if inner.entries.is_empty() {
            return;
        }
        inner.entries.clear();
        self.persist(&inner);
    }

    /// How many entries are held, whether or not history is on. For tests and diagnostics.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Write the current history back, remembering the failure if there is one. Called with the
    /// lock held, so the file always reflects the order in memory.
    fn persist(&self, inner: &Inner) {
        let Some(store) = &self.store else {
            return;
        };
        let history = History {
            enabled: inner.enabled,
            entries: inner.entries.clone(),
        };
        if let Err(error) = store.save(&history) {
            self.record_error(&error);
        }
    }

    fn record_error(&self, error: &PersistError) {
        *self
            .error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.reason().to_string());
    }

    /// A poisoned lock still holds consistent data here (no critical section can panic), so recover
    /// from it rather than panicking on a D-Bus thread.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::persist::FakeStore;

    /// The texts of the current history, newest first.
    fn texts(store: &ClipboardStore) -> Vec<String> {
        store.list().into_iter().map(|entry| entry.text).collect()
    }

    /// A store over a counting fake, as the daemon runs it over the encrypted file.
    fn persistent() -> (ClipboardStore, Arc<FakeStore>) {
        let fake = Arc::new(FakeStore::default());
        let store = ClipboardStore::open(fake.clone());
        store.set_enabled(true);
        (store, fake)
    }

    #[test]
    fn history_starts_off_and_records_nothing() {
        let store = ClipboardStore::new();
        assert!(!store.is_enabled());
        assert_eq!(store.push_with_outcome("anything", 1_000), PushOutcome::Disabled);
        assert!(store.list().is_empty());
        assert_eq!(store.len(), 0);
        assert_eq!(
            store.storage(),
            Storage::InMemory {
                reason: "no store is configured".to_string()
            }
        );
        assert!(!store.storage().is_persistent());
    }

    #[test]
    fn enabling_records_newest_first() {
        let store = ClipboardStore::new();
        store.set_enabled(true);

        assert_eq!(store.push_with_outcome("first", 1_000), PushOutcome::Recorded);
        assert!(store.push("second", 2_000));

        assert_eq!(texts(&store), ["second", "first"]);
        assert_eq!(store.list()[0].timestamp_ms, 2_000);
    }

    #[test]
    fn the_same_text_moves_to_the_front_instead_of_duplicating() {
        let store = ClipboardStore::new();
        store.set_enabled(true);

        store.push("same", 1_000);
        store.push("other", 1_500);
        store.push("same", 2_000);

        assert_eq!(texts(&store), ["same", "other"]);
        assert_eq!(store.list()[0].timestamp_ms, 2_000);
    }

    #[test]
    fn empty_text_is_not_an_entry() {
        let store = ClipboardStore::new();
        store.set_enabled(true);
        assert_eq!(store.push_with_outcome("", 1_000), PushOutcome::Empty);
        assert!(store.is_empty());
    }

    #[test]
    fn the_id_is_stable_for_the_same_text_and_differs_for_another() {
        let store = ClipboardStore::new();
        store.set_enabled(true);

        store.push("hello", 1_000);
        let first = store.list()[0].id.clone();
        store.push("hello", 2_000);
        store.push("hello there", 3_000);

        assert_eq!(entry_id("hello"), first);
        assert!(first.starts_with("clip:"));
        assert_ne!(store.list()[0].id, first, "other text, other id");
    }

    #[test]
    fn deleting_and_clearing() {
        let store = ClipboardStore::new();
        store.set_enabled(true);
        store.push("one", 1_000);
        store.push("two", 2_000);

        let victim = store.list()[1].id.clone();
        assert!(store.delete(&victim));
        assert!(!store.delete(&victim), "a second delete finds nothing");
        assert_eq!(store.list().len(), 1);

        store.clear();
        assert!(store.is_empty());
    }

    #[test]
    fn turning_history_off_hides_the_list_without_destroying_it() {
        let store = ClipboardStore::new();
        store.set_enabled(true);
        store.push("private", 1_000);

        store.set_enabled(false);
        assert!(store.list().is_empty(), "contract §9: no list while off");
        assert_eq!(
            store.push_with_outcome("while off", 2_000),
            PushOutcome::Disabled
        );

        store.set_enabled(true);
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].text, "private");
    }

    #[test]
    fn the_store_is_capped_and_drops_the_oldest() {
        let store = ClipboardStore::with_cap(2);
        store.set_enabled(true);
        store.push("one", 1_000);
        store.push("two", 2_000);
        store.push("three", 3_000);

        assert_eq!(texts(&store), ["three", "two"]);
    }

    #[test]
    fn the_store_is_shared_across_threads_without_corrupting_the_order() {
        use std::sync::Arc;

        let store = Arc::new(ClipboardStore::new());
        store.set_enabled(true);

        let writers: Vec<_> = (0..4)
            .map(|worker| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    for copy in 0..25 {
                        store.push(&format!("{worker}:{copy}"), i64::from(copy));
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().expect("no thread panics");
        }

        let list = store.list();
        assert_eq!(list.len(), 100, "every distinct copy is kept");
        let mut ids: Vec<&str> = list.iter().map(|e| e.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 100, "not one entry is stored twice");
    }

    // ---- ticket 11: persistence ----

    #[test]
    fn opening_a_store_loads_what_was_written_and_reports_where() {
        let (store, fake) = persistent();
        store.push("a surviving copy", 1_000);

        // A second store over the same (fake) file, as after a restart.
        let reloaded = ClipboardStore::open(fake.clone());

        assert!(reloaded.is_enabled(), "history comes back on");
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.list()[0].text, "a surviving copy");
        assert_eq!(reloaded.list()[0].timestamp_ms, 1_000);
        assert_eq!(
            reloaded.list()[0].id,
            entry_id("a surviving copy"),
            "the id is derived, so it survives too"
        );
        assert!(reloaded.storage().is_persistent());
        assert!(reloaded.error().is_none());
    }

    #[test]
    fn every_change_that_alters_the_history_is_written() {
        let (store, fake) = persistent();
        assert_eq!(fake.saved.lock().unwrap().len(), 1, "enabling is a change");

        store.push("one", 1_000);
        assert_eq!(fake.saved.lock().unwrap().len(), 2);

        store.push("secret=hunter2", 1_100);
        assert_eq!(
            fake.saved.lock().unwrap().len(),
            2,
            "a suppressed copy changes nothing, so nothing is written"
        );

        store.push("two", 1_200);
        let id = store.list()[0].id.clone();
        store.delete(&id);
        store.clear();
        assert_eq!(fake.saved.lock().unwrap().len(), 5);

        let last = fake.saved.lock().unwrap().last().cloned().expect("saved");
        assert!(last.entries.is_empty(), "the clear is what was written last");
    }

    #[test]
    fn reads_that_change_nothing_do_not_write() {
        let (store, fake) = persistent();
        let before = fake.saved.lock().unwrap().len();

        store.list();
        store.is_enabled();
        store.len();
        store.delete("clip:0000000000000000");
        store.set_enabled(true); // already on

        assert_eq!(fake.saved.lock().unwrap().len(), before);
    }

    #[test]
    fn a_history_that_cannot_be_read_degrades_to_memory_with_the_reason() {
        let fake = Arc::new(FakeStore {
            load_error: Some("the file could not be decrypted with this key".to_string()),
            ..FakeStore::default()
        });

        let store = ClipboardStore::open(fake);

        assert!(!store.storage().is_persistent());
        let reason = match store.storage() {
            Storage::InMemory { reason } => reason,
            other => panic!("expected a memory store, got {other:?}"),
        };
        assert!(reason.contains("could not be decrypted"), "{reason}");
        assert!(!store.is_enabled(), "a degraded store starts off");

        // And it still works, in memory, without writing anything back.
        store.set_enabled(true);
        assert!(store.push("still recorded", 1_000));
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn a_write_that_fails_keeps_the_entry_and_reports_the_reason() {
        let fake = Arc::new(FakeStore {
            save_error: Some("no space left on device".to_string()),
            ..FakeStore::default()
        });
        let store = ClipboardStore::open(fake);
        store.set_enabled(true);

        assert!(store.push("still in memory", 1_000));
        assert_eq!(store.list().len(), 1, "a failed write loses nothing");
        assert_eq!(
            store.error().as_deref(),
            Some("no space left on device"),
            "and says so"
        );
    }

    // ---- ticket 11: secret suppression ----

    #[test]
    fn secret_looking_text_is_suppressed_by_default_and_never_stored() {
        let (store, fake) = persistent();

        assert_eq!(
            store.push_with_outcome("password=hunter2", 1_000),
            PushOutcome::Suppressed
        );
        assert!(!store.push("-----BEGIN RSA PRIVATE KEY-----", 1_100));
        assert!(store.is_empty(), "nothing was recorded");
        assert!(
            fake.saved.lock().unwrap().iter().all(|history| history.entries.is_empty()),
            "and nothing was written"
        );

        // Ordinary text is unaffected.
        assert!(store.push("the quick brown fox", 2_000));
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn suppression_can_be_turned_off() {
        let store = ClipboardStore::new();
        store.set_enabled(true);
        assert!(store.suppresses_secrets(), "on by default");

        store.set_suppress_secrets(false);
        assert!(store.push("password=hunter2", 1_000));
        assert_eq!(store.list().len(), 1);
    }
}
