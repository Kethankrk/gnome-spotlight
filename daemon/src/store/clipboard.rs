//! The in-memory clipboard history.
//!
//! History is off until the user turns it on, it is newest-first, and the same text is never stored
//! twice. **Nothing here touches the filesystem** — persistence, encryption and secret suppression
//! are ticket 11, and this module is the seam that ticket replaces.
//!
//! The store is shared by the D-Bus dispatch thread (property reads, `ClipboardPush` and friends)
//! and the search thread (the clipboard provider), so it synchronises internally.

use std::sync::{Mutex, MutexGuard};

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

#[derive(Debug)]
struct Inner {
    enabled: bool,
    cap: usize,
    /// Newest first.
    entries: Vec<ClipEntry>,
}

/// Clipboard history, in memory only.
#[derive(Debug)]
pub struct ClipboardStore {
    inner: Mutex<Inner>,
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

    /// A fresh store: history off, nothing recorded.
    pub fn new() -> Self {
        Self::with_cap(Self::DEFAULT_CAP)
    }

    pub fn with_cap(cap: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                enabled: false,
                cap,
                entries: Vec::new(),
            }),
        }
    }

    /// Whether copies are being recorded.
    pub fn is_enabled(&self) -> bool {
        self.lock().enabled
    }

    /// Turn recording on or off.
    ///
    /// Turning history off stops new entries and hides the list (contract §9) but keeps what was
    /// recorded: it is memory-only, it dies with the process, and it is what the user just asked to
    /// see. `ClipboardClear` is the way to destroy it.
    pub fn set_enabled(&self, enabled: bool) {
        self.lock().enabled = enabled;
    }

    /// Record `text` as the newest entry.
    ///
    /// Ignored — and reported as `false` — while history is off, or for empty text. Text already in
    /// the history moves to the front with its new timestamp instead of appearing twice.
    pub fn push(&self, text: &str, timestamp_ms: i64) -> bool {
        let mut inner = self.lock();
        if !inner.enabled || text.is_empty() {
            return false;
        }

        let entry = ClipEntry::new(text, timestamp_ms);
        inner.entries.retain(|existing| existing.id != entry.id);
        inner.entries.insert(0, entry);
        let cap = inner.cap;
        inner.entries.truncate(cap);
        true
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
        inner.entries.len() != before
    }

    /// Drop every entry.
    pub fn clear(&self) {
        self.lock().entries.clear();
    }

    /// How many entries are held, whether or not history is on. For tests and diagnostics.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A poisoned lock still holds consistent data here (every critical section is infallible), so
    /// recover from it rather than panicking on a D-Bus thread.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The texts of the current history, newest first.
    fn texts(store: &ClipboardStore) -> Vec<String> {
        store.list().into_iter().map(|entry| entry.text).collect()
    }

    #[test]
    fn history_starts_off_and_records_nothing() {
        let store = ClipboardStore::new();
        assert!(!store.is_enabled());
        assert!(!store.push("anything", 1_000));
        assert!(store.list().is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn enabling_records_newest_first() {
        let store = ClipboardStore::new();
        store.set_enabled(true);

        assert!(store.push("first", 1_000));
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
        assert!(!store.push("", 1_000));
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
        assert!(!store.push("while off", 2_000));

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
}
