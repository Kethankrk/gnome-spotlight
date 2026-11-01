//! The cipher that keeps clipboard history unreadable at rest.
//!
//! ChaCha20-Poly1305 (RFC 8439) — an authenticated cipher, so a blob that was altered (or sealed
//! with another key) is rejected instead of being decrypted into garbage. The plaintext never
//! touches the disk: [`seal`] returns exactly the bytes that are written.
//!
//! **Blob format** (all lengths fixed, so a truncated file is detectable before decryption):
//!
//! ```text
//!   0   .. 11  MAGIC           `LUMEN-CLIP\x01`, also the associated data of every blob
//!  11   .. 23  nonce           12 fresh random bytes per write, never reused
//!  23   .. -16 ciphertext      ChaCha20 keystream over the plaintext
//!  -16  ..    tag              Poly1305, over MAGIC ‖ ciphertext
//! ```
//!
//! The magic doubles as the format version: a blob written by a later format will not open, and
//! because it is authenticated a blob that was mangled into a different header does not open
//! either. Randomness comes from `/dev/urandom` rather than a PRNG dependency — the daemon is
//! Linux-only (GNOME 50/51 are) and this keeps the dependency list to the cipher itself.

use std::fs::File;
use std::io::{self, Read};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

/// Format magic, and the associated data of every blob.
pub const MAGIC: &[u8; 11] = b"LUMEN-CLIP\x01";

/// ChaCha20-Poly1305's nonce length, per RFC 8439 §2.8.
pub const NONCE_LEN: usize = 12;
/// Poly1305's tag length.
pub const TAG_LEN: usize = 16;
/// Magic plus nonce: everything before the ciphertext.
pub const HEADER_LEN: usize = MAGIC.len() + NONCE_LEN;
/// The shortest blob that can be one of ours: a header and a bare tag.
pub const MIN_BLOB_LEN: usize = HEADER_LEN + TAG_LEN;

/// Why a blob could not be sealed or opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CipherError {
    /// Shorter than [`MIN_BLOB_LEN`]: truncated, or never written by us.
    Short,
    /// The magic does not match: another file, or another format version.
    NotOurs,
    /// Authentication failed — the wrong key, or the blob was altered.
    Tampered,
    /// The nonce could not be generated.
    Random(String),
}

impl CipherError {
    /// A phrase fit for a log line or a status message.
    pub fn reason(&self) -> String {
        match self {
            Self::Short => "the file is too short to be clipboard history".to_string(),
            Self::NotOurs => "the file is not a Lumen clipboard history".to_string(),
            Self::Tampered => "the file could not be decrypted with this key".to_string(),
            Self::Random(message) => format!("no randomness available: {message}"),
        }
    }
}

impl std::fmt::Display for CipherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason())
    }
}

/// Encrypt `plaintext` under `key`, returning the bytes to write to disk.
pub fn seal(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, CipherError> {
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce).map_err(|e| CipherError::Random(e.to_string()))?;

    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let sealed = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &MAGIC[..],
            },
        )
        // Encryption cannot fail for a key and nonce of the right size; report it as tampering
        // rather than panicking if the library ever disagrees.
        .map_err(|_| CipherError::Tampered)?;

    let mut blob = Vec::with_capacity(HEADER_LEN + sealed.len());
    blob.extend_from_slice(MAGIC);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&sealed);
    Ok(blob)
}

/// Decrypt a blob produced by [`seal`]. Any change to the blob, and any other key, is an error.
pub fn open(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>, CipherError> {
    if blob.len() < MIN_BLOB_LEN {
        return Err(CipherError::Short);
    }
    if &blob[..MAGIC.len()] != MAGIC {
        return Err(CipherError::NotOurs);
    }

    let (nonce, ciphertext) = blob[MAGIC.len()..].split_at(NONCE_LEN);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &MAGIC[..],
            },
        )
        .map_err(|_| CipherError::Tampered)
}

/// Fill `buf` with kernel randomness.
pub fn fill_random(buf: &mut [u8]) -> io::Result<()> {
    File::open("/dev/urandom")?.read_exact(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7u8; 32];

    #[test]
    fn a_blob_round_trips() {
        let blob = seal(&KEY, b"hello history").expect("seal");
        assert_eq!(open(&KEY, &blob).expect("open"), b"hello history");
    }

    #[test]
    fn an_empty_plaintext_round_trips_too() {
        let blob = seal(&KEY, b"").expect("seal");
        assert_eq!(blob.len(), MIN_BLOB_LEN, "header plus a bare tag");
        assert_eq!(open(&KEY, &blob).expect("open"), b"");
    }

    #[test]
    fn the_blob_starts_with_the_magic() {
        let blob = seal(&KEY, b"x").expect("seal");
        assert_eq!(&blob[..MAGIC.len()], MAGIC);
        assert_eq!(blob.len(), HEADER_LEN + 1 + TAG_LEN);
    }

    #[test]
    fn every_write_uses_a_fresh_nonce() {
        let first = seal(&KEY, b"same plaintext").expect("seal");
        let second = seal(&KEY, b"same plaintext").expect("seal");

        assert_ne!(first, second, "a reused nonce would be a break, not a nit");
        assert_ne!(
            &first[MAGIC.len()..HEADER_LEN],
            &second[MAGIC.len()..HEADER_LEN]
        );
        assert_eq!(open(&KEY, &first).unwrap(), open(&KEY, &second).unwrap());
    }

    #[test]
    fn the_plaintext_is_not_in_the_blob() {
        let plaintext = b"the user's private note";
        let blob = seal(&KEY, plaintext).expect("seal");

        assert!(
            !blob.windows(plaintext.len()).any(|w| w == plaintext),
            "the ciphertext must not contain the plaintext"
        );
    }

    #[test]
    fn another_key_does_not_open_it() {
        let blob = seal(&KEY, b"secret").expect("seal");
        assert_eq!(open(&[8u8; 32], &blob), Err(CipherError::Tampered));
    }

    #[test]
    fn tampering_is_detected() {
        let blob = seal(&KEY, b"a longer piece of text so there is something to flip").expect("seal");

        for index in [MAGIC.len(), HEADER_LEN, blob.len() - 1] {
            let mut altered = blob.clone();
            altered[index] ^= 0x01;
            assert_eq!(
                open(&KEY, &altered),
                Err(CipherError::Tampered),
                "flipping byte {index} must not decrypt"
            );
        }
    }

    #[test]
    fn a_different_magic_is_not_ours_and_is_not_authenticated() {
        let mut blob = seal(&KEY, b"text").expect("seal");
        blob[0] = b'X';
        assert_eq!(open(&KEY, &blob), Err(CipherError::NotOurs));
    }

    #[test]
    fn a_short_blob_is_rejected_before_it_is_decrypted() {
        let blob = seal(&KEY, b"text").expect("seal");
        for len in [0, 1, MAGIC.len(), HEADER_LEN, MIN_BLOB_LEN - 1] {
            assert_eq!(
                open(&KEY, &blob[..len]),
                Err(CipherError::Short),
                "{len} bytes is too short"
            );
        }
    }

    #[test]
    fn a_truncated_blob_is_rejected() {
        let blob = seal(&KEY, b"text").expect("seal");
        assert_eq!(open(&KEY, &blob[..blob.len() - 1]), Err(CipherError::Tampered));
    }

    #[test]
    fn randomness_is_random() {
        let mut first = [0u8; 32];
        let mut second = [0u8; 32];
        fill_random(&mut first).expect("urandom");
        fill_random(&mut second).expect("urandom");
        assert_ne!(first, second);
        assert_ne!(first, [0u8; 32]);
    }
}
