//! Store seams: the clipboard history, where its key comes from, and where it is written.
//!
//! Nothing outside this module knows that the history is encrypted, or that a keyring is involved;
//! the D-Bus surface only ever sees [`ClipboardStore`]. That is what lets the daemon's tests run
//! without a keyring, without a data directory and without a disk.

pub mod cipher;
pub mod clipboard;
pub mod keyring;
pub mod persist;
pub mod redact;

use std::path::PathBuf;
use std::sync::Arc;

pub use clipboard::{ClipEntry, ClipboardStore, PushOutcome, Storage};
pub use keyring::{SecretKey, SecretKeySource};
pub use persist::{EncryptedHistoryFile, History, HistoryStore, PersistError};

/// How the daemon keeps clipboard history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipboardMode {
    /// Encrypted, on disk, under a key in the login keyring — and in memory only, with a reason,
    /// when either is unavailable. The default.
    #[default]
    Persistent,
    /// Never touch the disk. What the test harness and `--clipboard memory` ask for.
    Memory,
}

/// The clipboard history the daemon serves: persistent when it can be, in memory when it cannot,
/// and never a reason to fail to start.
pub fn open_clipboard(mode: ClipboardMode) -> Arc<ClipboardStore> {
    match mode {
        ClipboardMode::Memory => Arc::new(ClipboardStore::new()),
        ClipboardMode::Persistent => {
            let store = match persist::history_path() {
                Some(path) => open_persistent_at(path, keyring::key_source().as_ref()),
                None => Arc::new(ClipboardStore::degraded(
                    "there is no data directory (set XDG_DATA_HOME or HOME)".to_string(),
                )),
            };
            if !store.storage().is_persistent() {
                // One line, once, at start-up: a user who wonders where their history went can
                // find it in the journal.
                eprintln!("lumen-daemon: {}", store.storage().describe());
            }
            store
        }
    }
}

/// Open the encrypted history at `path`, under the key `source` hands over.
///
/// Every failure is a reason the history is in memory only: no key from the keyring, no readable
/// or writable file. The daemon keeps serving either way.
fn open_persistent_at(path: PathBuf, source: &dyn SecretKeySource) -> Arc<ClipboardStore> {
    let key = match source.key() {
        Ok(key) => key,
        Err(error) => {
            return Arc::new(ClipboardStore::degraded(format!(
                "{} is unavailable: {}",
                source.describe(),
                error.reason()
            )))
        }
    };

    Arc::new(ClipboardStore::open(Arc::new(EncryptedHistoryFile::new(
        path, key,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring::FileKeySource;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumen-store-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Every file under `dir`, with its bytes — for looking for plaintext on the disk.
    fn files_on_disk(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        let mut found = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    found.push((path.display().to_string(), bytes));
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn memory_mode_never_touches_the_disk() {
        let store = open_clipboard(ClipboardMode::Memory);
        store.set_enabled(true);
        store.push("anything at all", 1_000);

        assert!(!store.storage().is_persistent());
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn a_persistent_store_round_trips_through_a_fake_key_source() {
        let dir = temp_dir("round-trip");
        let path = dir.join(persist::FILE_NAME);
        let source = FileKeySource::new(dir.join("key"));

        let store = open_persistent_at(path.clone(), &source);
        assert!(store.storage().is_persistent(), "{:?}", store.storage());
        store.set_enabled(true);
        store.push("the older copy", 1_000);
        store.push("the newest copy", 2_000);
        store.push("password=hunter2", 1_500); // suppressed, so never on disk
        let before = store.list();

        // A second daemon start: a new store, the same key file, the same history file.
        let reloaded = open_persistent_at(path.clone(), &FileKeySource::new(dir.join("key")));

        assert!(reloaded.storage().is_persistent());
        assert!(reloaded.is_enabled(), "history comes back on");
        assert_eq!(reloaded.list(), before);
        assert_eq!(
            reloaded.list().iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["the newest copy", "the older copy"],
            "and the suppressed copy is not there"
        );

        // Nothing on the disk is readable: no text, and no JSON of it.
        let files = files_on_disk(&dir);
        assert_eq!(files.len(), 2, "just the key and the history: {files:?}");
        let history_bytes = files
            .iter()
            .find(|(name, _)| name.ends_with(persist::FILE_NAME))
            .map(|(_, bytes)| bytes.clone())
            .expect("the history file");
        for needle in ["the newest copy", "the older copy", "entries", "timestamp_ms"] {
            assert!(
                !history_bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes()),
                "`{needle}` is on disk in the clear"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_corrupt_key_file_degrades_to_memory_with_the_reason() {
        let dir = temp_dir("corrupt-key");
        std::fs::create_dir_all(&dir).expect("directory");
        let key_path = dir.join("key");
        std::fs::write(&key_path, "deadbeef").expect("a short key file");
        let path = dir.join(persist::FILE_NAME);

        let store = open_persistent_at(path.clone(), &FileKeySource::new(&key_path));

        assert!(!store.storage().is_persistent());
        assert!(
            store.storage().describe().contains("hex characters"),
            "the reason names the problem: {}",
            store.storage().describe()
        );

        // And it still records, in memory, without creating a history file.
        store.set_enabled(true);
        assert!(store.push("recorded in memory", 1_000));
        assert_eq!(store.list().len(), 1);
        assert!(!path.exists(), "a degraded store must not write anything");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unreadable_key_file_degrades_without_panicking() {
        let dir = temp_dir("key-is-a-directory");
        std::fs::create_dir_all(dir.join("key")).expect("a directory where the key should be");

        let store = open_persistent_at(dir.join(persist::FILE_NAME), &FileKeySource::new(dir.join("key")));

        assert!(!store.storage().is_persistent());
        assert!(store.storage().describe().contains("in memory only"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_history_written_under_one_key_cannot_be_read_under_another() {
        let dir = temp_dir("two-keys");
        let history = dir.join(persist::FILE_NAME);

        let first = open_persistent_at(history.clone(), &FileKeySource::new(dir.join("key-one")));
        first.set_enabled(true);
        first.push("written under key one", 1_000);
        assert!(first.storage().is_persistent());

        let second = open_persistent_at(history.clone(), &FileKeySource::new(dir.join("key-two")));

        assert!(
            !second.storage().is_persistent(),
            "a key that does not decrypt the history degrades to memory"
        );
        assert!(second.list().is_empty());
        assert!(
            history.exists(),
            "and the unreadable file is left alone, not overwritten"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
