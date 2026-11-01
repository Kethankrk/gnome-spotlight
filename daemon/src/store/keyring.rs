//! Where the clipboard history's encryption key comes from.
//!
//! The key lives in the user's login keyring — a Secret Service provider (`gnome-keyring`) reached
//! over D-Bus at `org.freedesktop.secrets` — so it is protected by the login password and is *not*
//! on disk next to the history it decrypts. The daemon cannot see a locked keyring, and must never
//! take the session down when it cannot: every failure here is a [`SecretError`], which the store
//! turns into "history in memory only, and here is why" (contract §9).
//!
//! [`SecretKeySource`] is the seam. [`KeyringKeySource`] is the real one; a file under a
//! throw-away path ([`FileKeySource`]) is the test double, so the store's tests and the
//! integration harness never need a keyring.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use zbus::names::BusName;
use zbus::{Connection, OwnedObjectPath, OwnedVariant, Proxy, Variant};

use crate::store::cipher;

/// Key length: 32 bytes, what ChaCha20-Poly1305 takes.
pub const KEY_LEN: usize = 32;

/// The Secret Service's well-known name.
pub const SECRET_SERVICE: &str = "org.freedesktop.secrets";
const SECRETS_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_INTERFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_INTERFACE: &str = "org.freedesktop.Secret.Collection";
/// The item's label in the keyring, and the attributes it is found by.
pub const KEY_LABEL: &str = "Lumen clipboard history";
/// `(application, purpose)` — what every lookup searches by.
pub const KEY_ATTRIBUTES: [(&str, &str); 2] = [("application", "lumen"), ("purpose", "clipboard-history")];

/// A failure to obtain the key. Both variants degrade the same way; they differ in what the user
/// has to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// There is no key: no secrets service, a locked keyring, a dismissed prompt.
    Unavailable(String),
    /// There is a key, but it is not one: the wrong length, or not hex.
    Malformed(String),
}

impl SecretError {
    /// A phrase fit for a log line: complete enough to act on without the variant name.
    pub fn reason(&self) -> &str {
        match self {
            Self::Unavailable(message) | Self::Malformed(message) => message,
        }
    }
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason())
    }
}

/// The 32 bytes the clipboard history is encrypted under.
///
/// `Debug` redacts it and `Drop` wipes it, so the key cannot reach a log line or a core dump by
/// accident. It is deliberately not `Serialize`.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey([u8; KEY_LEN]);

impl SecretKey {
    /// Wrap raw bytes.
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// A fresh key from the kernel's randomness.
    pub fn generate() -> Result<Self, SecretError> {
        let mut bytes = [0u8; KEY_LEN];
        cipher::fill_random(&mut bytes)
            .map_err(|e| SecretError::Unavailable(format!("no randomness available: {e}")))?;
        Ok(Self(bytes))
    }

    /// Read a key from its hex spelling — the form the key is stored in, and the form the test
    /// seam takes. Leading and trailing whitespace is ignored; anything that is not exactly
    /// [`KEY_LEN`] bytes of hex is [`SecretError::Malformed`].
    pub fn from_hex(text: &str) -> Result<Self, SecretError> {
        let trimmed = text.trim();
        if trimmed.len() % 2 != 0 || trimmed.len() != KEY_LEN * 2 {
            return Err(SecretError::Malformed(format!(
                "the key must be {} hex characters, found {}",
                KEY_LEN * 2,
                trimmed.len()
            )));
        }

        let mut bytes = [0u8; KEY_LEN];
        for (slot, pair) in bytes.iter_mut().zip(trimmed.as_bytes().chunks(2)) {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            };
            match (digit(pair[0]), digit(pair[1])) {
                (Some(high), Some(low)) => *slot = (high << 4) | low,
                _ => {
                    return Err(SecretError::Malformed(
                        "the key is not hexadecimal".to_string(),
                    ))
                }
            }
        }
        Ok(Self(bytes))
    }

    /// The hex spelling written to a key file.
    pub fn to_hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(KEY_LEN * 2);
        for byte in self.0 {
            out.push(DIGITS[usize::from(byte >> 4)] as char);
            out.push(DIGITS[usize::from(byte & 0x0f)] as char);
        }
        out
    }

    /// The raw key, for the cipher.
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

impl Drop for SecretKey {
    fn drop(&mut self) {
        for byte in self.0.iter_mut() {
            // Volatile, so the write is not optimised away as a dead store.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// Where the encryption key comes from. One implementation talks to the login keyring; the others
/// are test doubles, which is why nothing below the store needs a keyring to be testable.
pub trait SecretKeySource: Send + Sync {
    /// How this source is spelled in a log line.
    fn describe(&self) -> String;

    /// The key, creating it on first use. Never panics: an unreachable keyring is an error.
    fn key(&self) -> Result<SecretKey, SecretError>;
}

/// Reads the key from a file, creating it on first use.
///
/// This is the test double for the login keyring, and it is also the `LUMEN_SECRET_KEY_FILE` seam:
/// a throw-away path lets the integration tests prove an encrypted round trip across a daemon
/// restart without touching the developer's keyring. A file that is present but is not a key is
/// [`SecretError::Malformed`] — the "corrupt or short key" degradation.
#[derive(Debug, Clone)]
pub struct FileKeySource {
    path: PathBuf,
}

impl FileKeySource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl SecretKeySource for FileKeySource {
    fn describe(&self) -> String {
        format!("the key file {}", self.path.display())
    }

    fn key(&self) -> Result<SecretKey, SecretError> {
        match fs::read_to_string(&self.path) {
            Ok(text) => SecretKey::from_hex(&text).map_err(|e| {
                SecretError::Malformed(format!("{}: {}", self.path.display(), e.reason()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = SecretKey::generate()?;
                write_key_file(&self.path, &key)?;
                Ok(key)
            }
            Err(e) => Err(SecretError::Unavailable(format!(
                "the key file {} cannot be read: {e}",
                self.path.display()
            ))),
        }
    }
}

/// A key handed over directly. Tests only; there is no environment spelling for it.
#[cfg(test)]
#[derive(Debug)]
pub struct FixedKeySource(pub SecretKey);

#[cfg(test)]
impl SecretKeySource for FixedKeySource {
    fn describe(&self) -> String {
        "a fixed key".to_string()
    }

    fn key(&self) -> Result<SecretKey, SecretError> {
        Ok(self.0.clone())
    }
}

/// The real source: a Secret Service item in the user's login keyring.
#[derive(Debug, Clone)]
pub struct KeyringKeySource {
    /// The bus name to talk to; overridable so a test can point at nothing.
    pub service: String,
    /// The item's label.
    pub label: String,
    /// The attributes the item is found (and created) by.
    pub attributes: Vec<(String, String)>,
}

impl Default for KeyringKeySource {
    fn default() -> Self {
        Self {
            service: SECRET_SERVICE.to_string(),
            label: KEY_LABEL.to_string(),
            attributes: KEY_ATTRIBUTES
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        }
    }
}

impl KeyringKeySource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the key from `org.freedesktop.secrets`, over the session bus.
    ///
    /// The item is created on first use. A keyring we cannot reach, a locked collection, and a
    /// prompt for the user to answer are all [`SecretError::Unavailable`]: a headless daemon has no
    /// way to ask, and the store degrades to memory instead.
    ///
    /// The name must already be owned before anything is called on it. A search daemon must not
    /// start the user's keyring behind their back, and a name that no one owns would otherwise cost
    /// the bus's whole activation timeout before failing.
    async fn key_over_dbus(&self) -> Result<SecretKey, SecretError> {
        let unavailable =
            |what: &str, e: zbus::Error| SecretError::Unavailable(format!("{what}: {e}"));

        let connection = Connection::session()
            .await
            .map_err(|e| unavailable("the session bus is not reachable", e))?;

        let service_name = BusName::try_from(self.service.as_str()).map_err(|_| {
            SecretError::Unavailable(format!("`{}` is not a bus name", self.service))
        })?;
        let bus = zbus::fdo::DBusProxy::new(&connection)
            .await
            .map_err(|e| unavailable("the session bus is not reachable", e))?;
        let owned = bus
            .name_has_owner(service_name)
            .await
            .map_err(|e| {
                unavailable(
                    "the session bus cannot be asked about the login keyring",
                    e.into(),
                )
            })?;
        if !owned {
            return Err(SecretError::Unavailable(format!(
                "{} is not running",
                self.service
            )));
        }

        let service = Proxy::new(
            &connection,
            self.service.as_str(),
            SECRETS_PATH,
            SERVICE_INTERFACE,
        )
        .await
        .map_err(|e| unavailable("the login keyring is not available", e))?;

        // A plain session needs no key exchange and is accepted on the session bus; the value
        // itself is encrypted by the store, so the transport adds nothing. The input is ignored by
        // the specification but validated by the implementations: gnome-keyring (50.0, verified on
        // the build host) rejects anything but a string here with
        // `org.freedesktop.DBus.Error.InvalidArgs: The session algorithm input argument (plain) was
        // invalid`, which is what `libsecret` sends — an empty string.
        let (_, session): (OwnedVariant, OwnedObjectPath) = service
            .call(
                "OpenSession",
                &(
                    "plain",
                    OwnedVariant::try_from(Variant::from(String::new()))
                        .map_err(|e| unavailable("the keyring session cannot be opened", e.into()))?,
                ),
            )
            .await
            .map_err(|e| unavailable("the keyring session cannot be opened", e))?;

        let attributes: std::collections::HashMap<String, String> =
            self.attributes.iter().cloned().collect();
        let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = service
            .call("SearchItems", &(attributes.clone(),))
            .await
            .map_err(|e| unavailable("the login keyring cannot be searched", e))?;

        let mut items = unlocked;
        if items.is_empty() && !locked.is_empty() {
            let (now_unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = service
                .call("Unlock", &(locked,))
                .await
                .map_err(|e| unavailable("the login keyring cannot be unlocked", e))?;
            if now_unlocked.is_empty() || !prompt.as_str().is_empty() && prompt.as_str() != "/" {
                return Err(SecretError::Unavailable(
                    "the login keyring is locked; unlock it and restart Lumen".to_string(),
                ));
            }
            items = now_unlocked;
        }

        if !items.is_empty() {
            let secrets: std::collections::HashMap<
                OwnedObjectPath,
                (OwnedObjectPath, Vec<u8>, Vec<u8>, String),
            > = service
                .call("GetSecrets", &(items.clone(), session.clone()))
                .await
                .map_err(|e| unavailable("the keyring item cannot be read", e))?;

            let value = items
                .iter()
                .find_map(|item| secrets.get(item))
                .map(|(_, _, value, _)| value.clone())
                .ok_or_else(|| {
                    SecretError::Unavailable("the keyring item holds no secret".to_string())
                })?;
            return SecretKey::from_hex(&String::from_utf8_lossy(&value)).map_err(|e| {
                SecretError::Malformed(format!("the keyring item is not a Lumen key: {}", e.reason()))
            });
        }

        // Nothing stored yet: make one.
        let key = SecretKey::generate()?;
        let (collection,): (OwnedObjectPath,) = service
            .call("ReadAlias", &("default",))
            .await
            .map_err(|e| unavailable("the login keyring cannot be reached", e))?;
        if collection.as_str().is_empty() || collection.as_str() == "/" {
            return Err(SecretError::Unavailable(
                "there is no default keyring collection".to_string(),
            ));
        }

        let collection_proxy = Proxy::new(
            &connection,
            self.service.as_str(),
            collection.as_str(),
            COLLECTION_INTERFACE,
        )
        .await
        .map_err(|e| unavailable("the login keyring collection is not available", e))?;

        let mut properties: std::collections::HashMap<String, OwnedVariant> =
            std::collections::HashMap::new();
        let variant = |value: Variant<'_>| {
            OwnedVariant::try_from(value)
                .map_err(|e| unavailable("the keyring item cannot be described", e.into()))
        };
        properties.insert(
            "org.freedesktop.Secret.Item.Label".to_string(),
            variant(Variant::from(self.label.clone()))?,
        );
        properties.insert(
            "org.freedesktop.Secret.Item.Attributes".to_string(),
            variant(Variant::from(attributes))?,
        );

        let secret = (
            session.clone(),
            Vec::<u8>::new(),
            key.to_hex().into_bytes(),
            "text/plain".to_string(),
        );
        let (item, item_prompt): (OwnedObjectPath, OwnedObjectPath) = collection_proxy
            .call("CreateItem", &(properties, secret, true))
            .await
            .map_err(|e| unavailable("the key cannot be stored in the login keyring", e))?;

        if item.as_str().is_empty() || item.as_str() == "/" {
            return Err(SecretError::Unavailable(format!(
                "the login keyring wants to ask about storing the Lumen key (prompt {})",
                item_prompt.as_str()
            )));
        }
        Ok(key)
    }
}

impl SecretKeySource for KeyringKeySource {
    fn describe(&self) -> String {
        format!("the {} keyring item `{}`", self.service, self.label)
    }

    fn key(&self) -> Result<SecretKey, SecretError> {
        // The daemon's start-up path is already inside a `zbus::block_on`, and zbus cannot nest
        // its own runtime, so the secret-service round trip runs on a thread of its own.
        std::thread::scope(|scope| match scope.spawn(|| zbus::block_on(self.key_over_dbus())).join() {
            Ok(result) => result,
            Err(_) => Err(SecretError::Unavailable(
                "the login keyring client failed".to_string(),
            )),
        })
    }
}

/// Write a key file that only its owner can read, creating its directory if need be.
fn write_key_file(path: &Path, key: &SecretKey) -> Result<(), SecretError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            SecretError::Unavailable(format!(
                "the key file directory {} cannot be created: {e}",
                parent.display()
            ))
        })?;
        set_mode(parent, 0o700);
    }
    fs::write(path, key.to_hex()).map_err(|e| {
        SecretError::Unavailable(format!(
            "the key file {} cannot be written: {e}",
            path.display()
        ))
    })?;
    set_mode(path, 0o600);
    Ok(())
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

/// Resolve the key every way the daemon knows, in the order the environment asks for.
pub fn key_source() -> Box<dyn SecretKeySource> {
    if let Some(path) = std::env::var_os(KEY_FILE_ENV) {
        return Box::new(FileKeySource::new(PathBuf::from(path)));
    }
    Box::new(KeyringKeySource::new())
}

/// Point the daemon at a key file instead of the login keyring (testing seam, like
/// `LUMEN_FAKE_INDEX`).
pub const KEY_FILE_ENV: &str = "LUMEN_SECRET_KEY_FILE";

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lumen-key-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_file(&path);
        path
    }

    #[test]
    fn hex_round_trips_and_is_redacted_in_debug() {
        let key = SecretKey::from_bytes([0xab; KEY_LEN]);
        let hex = key.to_hex();
        assert_eq!(hex.len(), KEY_LEN * 2);
        assert_eq!(SecretKey::from_hex(&hex).expect("parse"), key);
        assert_eq!(format!("{key:?}"), "SecretKey(<redacted>)");
        assert!(!format!("{key:?}").contains("ab"));
    }

    #[test]
    fn a_key_may_be_uppercase_and_padded_with_whitespace() {
        let key = SecretKey::from_bytes([0x01; KEY_LEN]);
        let upper = key.to_hex().to_uppercase();
        assert_eq!(SecretKey::from_hex(&format!("  {upper}\n")).unwrap(), key);
    }

    #[test]
    fn a_short_or_corrupt_key_is_malformed_never_a_panic() {
        for text in ["", "deadbeef", "00", "not hex at all", &"a".repeat(63), &"a".repeat(65)] {
            let error = SecretKey::from_hex(text).expect_err("must not parse");
            assert!(
                matches!(error, SecretError::Malformed(_)),
                "`{text}` must be malformed, got {error:?}"
            );
            assert!(!error.reason().is_empty());
        }
    }

    #[test]
    fn a_key_file_is_created_once_and_read_back_afterwards() {
        let path = temp_path("key-file");
        let source = FileKeySource::new(&path);

        let first = source.key().expect("the first call creates the key");
        assert!(path.is_file());
        assert_eq!(
            fs::read_to_string(&path).expect("the key is written as hex").trim(),
            first.to_hex()
        );

        let second = source.key().expect("the second call reads the same key");
        assert_eq!(first, second, "a restart must find the same key");

        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the key file is owner-only");

        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_corrupt_key_file_degrades_instead_of_panicking() {
        let path = temp_path("corrupt-key");
        fs::write(&path, "short").expect("write a short key");

        let error = FileKeySource::new(&path).key().expect_err("must not parse");
        assert!(
            matches!(error, SecretError::Malformed(_)),
            "a short key file is malformed, got {error:?}"
        );
        assert!(error.reason().contains("64 hex characters"), "{}", error.reason());

        fs::remove_file(&path).ok();
    }

    #[test]
    fn an_unreadable_key_file_is_unavailable() {
        let path = temp_path("directory-not-a-file");
        fs::create_dir_all(&path).expect("a directory where the key should be");

        let error = FileKeySource::new(&path).key().expect_err("a directory is not a key");
        assert!(matches!(error, SecretError::Unavailable(_)), "got {error:?}");

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn a_keyring_that_is_not_there_is_unavailable_not_a_panic() {
        let source = KeyringKeySource {
            // A bus name nobody owns: the same path a machine without a keyring takes.
            service: format!("org.gnome.Lumen.Test.NoKeyring{}", std::process::id()),
            ..KeyringKeySource::default()
        };

        match source.key() {
            Err(SecretError::Unavailable(reason)) => assert!(!reason.is_empty()),
            // A machine with no session bus at all is the same story.
            Err(SecretError::Malformed(reason)) => panic!("unexpected: {reason}"),
            Ok(_) => panic!("a name nobody owns cannot hand out a key"),
        }
    }

    #[test]
    fn the_default_source_is_the_login_keyring() {
        // Only when the seam is not set; the environment of the test runner is what it is.
        if std::env::var_os(KEY_FILE_ENV).is_none() {
            assert_eq!(key_source().describe(), format!("the {SECRET_SERVICE} keyring item `{KEY_LABEL}`"));
        }
    }
}
