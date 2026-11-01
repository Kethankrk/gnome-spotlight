//! Where the clipboard history is written down, and how it is kept unreadable.
//!
//! [`HistoryStore`] is the seam: the clipboard store knows only this, so its tests use an
//! in-memory double and never need a keyring, a data directory or a disk. [`EncryptedHistoryFile`]
//! is the real one — the JSON history is sealed with [`crate::store::cipher`] under the key from
//! [`crate::store::keyring`] and written atomically, owner-only, into the user's XDG data
//! directory.
//!
//! Nothing here ever writes plaintext: the string handed to `write` is the ciphertext.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::store::cipher::{self, CipherError};
use crate::store::clipboard::{ClipEntry, Storage};
use crate::store::keyring::SecretKey;

/// The file's name inside the `lumen` directory.
pub const FILE_NAME: &str = "clipboard.bin";
/// The directory inside the XDG data directory.
pub const DIR_NAME: &str = "lumen";
/// The payload's format version. A blob of another version is not read.
pub const FORMAT_VERSION: u64 = 1;

/// What is persisted: the entries, and whether recording was on when they were written.
///
/// History that survives a restart is only useful if it comes back *on* — a user who has to tick
/// the box again after every login would be forgiven for thinking it was lost.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    pub enabled: bool,
    /// Newest first.
    pub entries: Vec<ClipEntry>,
}

/// Why the history could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistError {
    /// The file could not be read or written.
    Io(String),
    /// The file is ours but the payload is not what this version writes.
    Corrupt(String),
    /// The bytes did not decrypt: another key, or an altered file.
    Cipher(String),
}

impl PersistError {
    /// A phrase fit for a log line.
    pub fn reason(&self) -> &str {
        match self {
            Self::Io(message) | Self::Corrupt(message) | Self::Cipher(message) => message,
        }
    }
}

impl From<CipherError> for PersistError {
    fn from(error: CipherError) -> Self {
        Self::Cipher(error.reason())
    }
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

/// Encrypted, on-disk storage for the clipboard history.
///
/// A store that cannot be read is an error, never silently empty: the daemon degrades to memory
/// and leaves the file alone, rather than overwriting history it could not decrypt.
pub trait HistoryStore: std::fmt::Debug + Send + Sync + 'static {
    /// The history as last written. A store that has never been written is empty, not an error.
    fn load(&self) -> Result<History, PersistError>;

    /// Replace the stored history.
    fn save(&self, history: &History) -> Result<(), PersistError>;

    /// How this store is spelled in a log line.
    fn describe(&self) -> String;

    /// How the history is being kept behind this store, for the diagnostics the store reports.
    fn storage(&self) -> Storage;
}

/// The history as a file encrypted under a login-keyring key.
#[derive(Debug)]
pub struct EncryptedHistoryFile {
    path: PathBuf,
    key: SecretKey,
}

impl EncryptedHistoryFile {
    pub fn new(path: impl Into<PathBuf>, key: SecretKey) -> Self {
        Self {
            path: path.into(),
            key,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The ciphertext, for a test that wants to look for plaintext in it.
    pub fn read_sealed(&self) -> Result<Vec<u8>, PersistError> {
        fs::read(&self.path).map_err(|e| {
            PersistError::Io(format!("{} cannot be read: {e}", self.path.display()))
        })
    }
}

impl HistoryStore for EncryptedHistoryFile {
    fn load(&self) -> Result<History, PersistError> {
        let blob = match fs::read(&self.path) {
            Ok(blob) => blob,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(History::default()),
            Err(e) => {
                return Err(PersistError::Io(format!(
                    "{} cannot be read: {e}",
                    self.path.display()
                )))
            }
        };

        let plaintext = cipher::open(self.key.as_bytes(), &blob)?;
        decode(&plaintext).map_err(|message| {
            PersistError::Corrupt(format!("{}: {message}", self.path.display()))
        })
    }

    fn save(&self, history: &History) -> Result<(), PersistError> {
        let plaintext = encode(history);
        let blob = cipher::seal(self.key.as_bytes(), &plaintext)?;
        write_atomically(&self.path, &blob)
    }

    fn describe(&self) -> String {
        format!("the encrypted history {}", self.path.display())
    }

    fn storage(&self) -> Storage {
        Storage::OnDisk {
            path: self.path.clone(),
        }
    }
}

/// The history as bytes, in the payload format this version reads back.
fn encode(history: &History) -> Vec<u8> {
    let entries: Vec<Value> = history
        .entries
        .iter()
        .map(|entry| json!({ "text": entry.text, "timestamp_ms": entry.timestamp_ms }))
        .collect();
    let payload = json!({
        "version": FORMAT_VERSION,
        "enabled": history.enabled,
        "entries": entries,
    });
    serde_json::to_vec(&payload).unwrap_or_default()
}

/// The reverse of [`encode`]. Ids are not stored: they are derived from the text (see
/// [`crate::store::clipboard::entry_id`]), so they cannot drift from it.
fn decode(plaintext: &[u8]) -> Result<History, String> {
    let payload: Value = serde_json::from_slice(plaintext)
        .map_err(|e| format!("the payload is not JSON: {e}"))?;

    let version = payload.get("version").and_then(Value::as_u64).unwrap_or(0);
    if version != FORMAT_VERSION {
        return Err(format!(
            "the payload is version {version}, this daemon reads version {FORMAT_VERSION}"
        ));
    }

    let enabled = payload
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let entries = payload
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| "the payload has no entries".to_string())?;

    let mut history = History {
        enabled,
        entries: Vec::with_capacity(entries.len()),
    };
    for entry in entries {
        let text = entry
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| "an entry has no text".to_string())?;
        let timestamp_ms = entry
            .get("timestamp_ms")
            .and_then(Value::as_i64)
            .ok_or_else(|| "an entry has no timestamp".to_string())?;
        history.entries.push(ClipEntry::new(text, timestamp_ms));
    }
    Ok(history)
}

/// Write `bytes` to `path` so that a reader either sees the old file or the whole new one.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), PersistError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|e| {
        PersistError::Io(format!("{} cannot be created: {e}", parent.display()))
    })?;
    set_mode(parent, 0o700);

    // A sibling, so the rename stays inside one filesystem and is therefore atomic.
    let temporary = path.with_file_name(match path.file_name() {
        Some(name) => {
            let mut name = name.to_os_string();
            name.push(".tmp");
            name
        }
        None => OsString::from("clipboard.bin.tmp"),
    });

    fs::write(&temporary, bytes).map_err(|e| {
        PersistError::Io(format!("{} cannot be written: {e}", temporary.display()))
    })?;
    set_mode(&temporary, 0o600);
    fs::rename(&temporary, path).map_err(|e| {
        let _ = fs::remove_file(&temporary);
        PersistError::Io(format!("{} cannot be replaced: {e}", path.display()))
    })
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

/// Where the history lives: `$XDG_DATA_HOME/lumen/clipboard.bin`, or
/// `~/.local/share/lumen/clipboard.bin`. `None` when the environment names no data directory,
/// which degrades to memory like any other missing prerequisite.
pub fn history_path() -> Option<PathBuf> {
    history_path_from(|name| std::env::var_os(name))
}

fn history_path_from(lookup: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let usable = |value: OsString| {
        let path = PathBuf::from(value);
        (!path.as_os_str().is_empty() && path.is_absolute()).then_some(path)
    };

    if let Some(data) = lookup("XDG_DATA_HOME").and_then(usable) {
        return Some(data.join(DIR_NAME).join(FILE_NAME));
    }
    let home = lookup("HOME").and_then(usable)?;
    Some(home.join(".local").join("share").join(DIR_NAME).join(FILE_NAME))
}

/// The in-memory double the clipboard store's tests use: no keyring, no disk, and it counts what
/// it was asked to do.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct FakeStore {
    pub saved: std::sync::Mutex<Vec<History>>,
    pub load_error: Option<String>,
    pub save_error: Option<String>,
}

#[cfg(test)]
impl HistoryStore for FakeStore {
    fn load(&self) -> Result<History, PersistError> {
        match &self.load_error {
            Some(message) => Err(PersistError::Corrupt(message.clone())),
            None => Ok(self
                .saved
                .lock()
                .expect("not poisoned")
                .last()
                .cloned()
                .unwrap_or_default()),
        }
    }

    fn save(&self, history: &History) -> Result<(), PersistError> {
        if let Some(message) = &self.save_error {
            return Err(PersistError::Io(message.clone()));
        }
        self.saved.lock().expect("not poisoned").push(history.clone());
        Ok(())
    }

    fn describe(&self) -> String {
        "a fake store".to_string()
    }

    fn storage(&self) -> Storage {
        Storage::OnDisk {
            path: PathBuf::from("<fake>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumen-history-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn key() -> SecretKey {
        SecretKey::from_bytes([0x5a; 32])
    }

    fn history() -> History {
        History {
            enabled: true,
            entries: vec![
                ClipEntry::new("the newest copy", 2_000),
                ClipEntry::new("the older copy", 1_000),
            ],
        }
    }

    #[test]
    fn a_save_reloads_identically() {
        let dir = temp_dir("round-trip");
        let file = EncryptedHistoryFile::new(dir.join(FILE_NAME), key());

        file.save(&history()).expect("save");

        let reloaded = EncryptedHistoryFile::new(dir.join(FILE_NAME), key())
            .load()
            .expect("load");
        assert_eq!(reloaded, history());
        assert_eq!(reloaded.entries[0].id, crate::store::clipboard::entry_id("the newest copy"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nothing_on_disk_is_plaintext() {
        let dir = temp_dir("no-plaintext");
        let file = EncryptedHistoryFile::new(dir.join(FILE_NAME), key());
        file.save(&history()).expect("save");

        let blob = file.read_sealed().expect("read the sealed file");
        for needle in ["the newest copy", "the older copy", "text", "timestamp_ms", "version"] {
            assert!(
                !blob.windows(needle.len()).any(|window| window == needle.as_bytes()),
                "the file contains `{needle}` in the clear"
            );
        }

        // And nothing else was left behind, not even a temporary file: plaintext never reached the
        // disk, and neither did a second copy of the ciphertext.
        let left: Vec<String> = fs::read_dir(&dir)
            .expect("the directory exists")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec![FILE_NAME.to_string()]);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_file_and_its_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("modes");
        let file = EncryptedHistoryFile::new(dir.join(FILE_NAME), key());
        file.save(&history()).expect("save");

        let file_mode = fs::metadata(dir.join(FILE_NAME)).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_history_that_was_never_written_is_empty() {
        let dir = temp_dir("missing");
        let file = EncryptedHistoryFile::new(dir.join(FILE_NAME), key());

        assert_eq!(file.load().expect("an absent file is an empty history"), History::default());
        assert!(!dir.exists(), "loading must not create anything");
    }

    #[test]
    fn another_key_does_not_read_it() {
        let dir = temp_dir("wrong-key");
        EncryptedHistoryFile::new(dir.join(FILE_NAME), key())
            .save(&history())
            .expect("save");

        let error = EncryptedHistoryFile::new(dir.join(FILE_NAME), SecretKey::from_bytes([0x07; 32]))
            .load()
            .expect_err("another key cannot read it");
        assert!(matches!(error, PersistError::Cipher(_)), "got {error:?}");
        assert!(!error.reason().is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_truncated_file_degrades_to_an_error_not_a_panic() {
        let dir = temp_dir("truncated");
        fs::create_dir_all(&dir).expect("directory");
        let file = EncryptedHistoryFile::new(dir.join(FILE_NAME), key());
        file.save(&history()).expect("save");

        let blob = file.read_sealed().expect("read");
        fs::write(dir.join(FILE_NAME), &blob[..blob.len() / 2]).expect("truncate");

        let error = file.load().expect_err("a truncated file is an error");
        assert!(matches!(error, PersistError::Cipher(_)), "got {error:?}");

        fs::write(dir.join(FILE_NAME), b"LUMEN-CLP\x01").expect("write a short header");
        assert!(matches!(file.load(), Err(PersistError::Cipher(_))));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_payload_of_the_wrong_shape_is_corrupt() {
        for plaintext in [
            &b"not json"[..],
            br#"{"version":99,"entries":[]}"#,
            br#"{"version":1}"#,
            br#"{"version":1,"entries":[]} trailing"#,
        ] {
            assert!(decode(plaintext).is_err(), "{:?} must be corrupt", plaintext);
        }

        let decoded = decode(br#"{"version":1,"enabled":true,"entries":[{"text":"x","timestamp_ms":5}]}"#)
            .expect("a well-formed payload");
        assert!(decoded.enabled);
        assert_eq!(decoded.entries.len(), 1);
        assert_eq!(decoded.entries[0].text, "x");
    }

    #[test]
    fn the_payload_round_trips_through_encode_and_decode() {
        assert_eq!(decode(&encode(&history())).expect("decode"), history());
    }

    #[test]
    fn the_data_directory_follows_the_xdg_specification() {
        let from = |pairs: &'static [(&'static str, &'static str)]| {
            history_path_from(move |name| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(*value))
            })
        };

        assert_eq!(
            from(&[("XDG_DATA_HOME", "/data"), ("HOME", "/home/me")]),
            Some(PathBuf::from("/data/lumen/clipboard.bin"))
        );
        assert_eq!(
            from(&[("HOME", "/home/me")]),
            Some(PathBuf::from("/home/me/.local/share/lumen/clipboard.bin")),
            "no XDG_DATA_HOME falls back to ~/.local/share"
        );
        assert_eq!(
            from(&[("XDG_DATA_HOME", "relative"), ("HOME", "/home/me")]),
            Some(PathBuf::from("/home/me/.local/share/lumen/clipboard.bin")),
            "a relative XDG_DATA_HOME is invalid and ignored"
        );
        assert_eq!(from(&[]), None, "no home is no data directory");
    }

    #[test]
    fn the_fake_store_is_a_history_store() {
        use std::sync::Arc;

        let fake = Arc::new(FakeStore::default());
        let store: Arc<dyn HistoryStore> = fake.clone();
        store.save(&history()).expect("save");
        assert_eq!(store.load().expect("load"), history());
        assert_eq!(fake.saved.lock().unwrap().len(), 1);
    }
}
