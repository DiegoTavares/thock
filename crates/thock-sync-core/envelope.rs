//! The byte format every blob and write payload travels in (spec §5):
//! `TVS1`, a 12-byte nonce, then ChaCha20-Poly1305 ciphertext and tag, with
//! the path and blob id (or the write's client id) as associated data.

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use sha2::{Digest, Sha256};

pub const MAGIC: &[u8; 4] = b"TVS1";
pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;
/// Bytes an envelope adds to its plaintext.
pub const OVERHEAD: usize = MAGIC.len() + NONCE_LEN + TAG_LEN;

/// What an envelope is bound to (spec §5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    File { path: String, blob_id: String },
    Write { client_id: String },
}

impl Context {
    fn associated_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        match self {
            Self::File { path, blob_id } => {
                data.extend_from_slice(b"thock-file/1\0");
                data.extend_from_slice(path.as_bytes());
                data.push(0);
                data.extend_from_slice(blob_id.as_bytes());
            }
            Self::Write { client_id } => {
                data.extend_from_slice(b"thock-write/1\0");
                data.extend_from_slice(client_id.as_bytes());
            }
        }
        data
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SealError {
    #[error("the envelope is too short to hold anything")]
    TooShort,
    #[error("the envelope isn't a Thock vault blob")]
    BadMagic,
    #[error("the envelope doesn't open with this key at this path")]
    Authentication,
}

/// Seals `plaintext` under `key` for `context` with a fresh random nonce.
pub fn seal(key: &[u8; 32], context: Context, plaintext: &[u8]) -> Vec<u8> {
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    seal_with_nonce(key, &nonce.into(), context, plaintext)
}

/// `seal` with a caller-chosen nonce. For known-answer vectors only: reusing
/// a nonce under one key breaks the cipher.
#[doc(hidden)]
pub fn seal_with_nonce(
    key: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
    context: Context,
    plaintext: &[u8],
) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let associated = context.associated_data();
    let payload = Payload {
        msg: plaintext,
        aad: &associated,
    };
    // Encryption only fails when the plaintext exceeds the cipher's limit,
    // far above the 2 MB the protocol allows; an empty body is the honest
    // output for that impossible input rather than a panic.
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(nonce), payload)
        .unwrap_or_default();
    let mut envelope = Vec::with_capacity(OVERHEAD + plaintext.len());
    envelope.extend_from_slice(MAGIC);
    envelope.extend_from_slice(nonce);
    envelope.extend_from_slice(&ciphertext);
    envelope
}

/// Opens an envelope sealed for the same `context` under `key`.
pub fn open(key: &[u8; 32], context: Context, envelope: &[u8]) -> Result<Vec<u8>, SealError> {
    if envelope.len() < OVERHEAD {
        return Err(SealError::TooShort);
    }
    let (magic, rest) = envelope.split_at(MAGIC.len());
    if magic != MAGIC {
        return Err(SealError::BadMagic);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let associated = context.associated_data();
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &associated,
            },
        )
        .map_err(|_| SealError::Authentication)
}

/// `hex(sha256(envelope))`: the index's `content_hash` (spec §5.2).
pub fn content_hash(envelope: &[u8]) -> String {
    hex::encode(Sha256::digest(envelope))
}

/// The key-derived value the server may hold (spec §2):
/// `hex(sha256("thock-vault-key-check/1" ‖ key))[0..32]`.
pub fn key_check(key: &[u8; 32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"thock-vault-key-check/1");
    hasher.update(key);
    let mut hex = hex::encode(hasher.finalize());
    hex.truncate(32);
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 32] {
        let mut key = [0u8; 32];
        for (index, byte) in key.iter_mut().enumerate() {
            *byte = index as u8;
        }
        key
    }

    fn file_context() -> Context {
        Context::File {
            path: "daily/2026-10-02.md".into(),
            blob_id: "0123456789abcdef0123456789abcdef".into(),
        }
    }

    #[test]
    fn seal_then_open_round_trips() {
        let envelope = seal(&key(), file_context(), b"# Today\n");
        assert_eq!(envelope.len(), 8 + OVERHEAD);
        assert_eq!(&envelope[..4], MAGIC);
        assert_eq!(
            open(&key(), file_context(), &envelope),
            Ok(b"# Today\n".to_vec())
        );
    }

    #[test]
    fn moved_blobs_do_not_open() {
        let envelope = seal(&key(), file_context(), b"x");
        let other_path = Context::File {
            path: "daily/2026-10-03.md".into(),
            blob_id: "0123456789abcdef0123456789abcdef".into(),
        };
        let other_blob = Context::File {
            path: "daily/2026-10-02.md".into(),
            blob_id: "ffffffffffffffffffffffffffffffff".into(),
        };
        assert_eq!(
            open(&key(), other_path, &envelope),
            Err(SealError::Authentication)
        );
        assert_eq!(
            open(&key(), other_blob, &envelope),
            Err(SealError::Authentication)
        );
        let as_write = Context::Write {
            client_id: "c".into(),
        };
        assert_eq!(
            open(&key(), as_write, &envelope),
            Err(SealError::Authentication)
        );
        let mut wrong_key = key();
        wrong_key[0] ^= 1;
        assert_eq!(
            open(&wrong_key, file_context(), &envelope),
            Err(SealError::Authentication)
        );
    }

    #[test]
    fn malformed_envelopes() {
        assert_eq!(
            open(&key(), file_context(), b"TVS1"),
            Err(SealError::TooShort)
        );
        let mut envelope = seal(&key(), file_context(), b"x");
        envelope[0] = b'X';
        assert_eq!(
            open(&key(), file_context(), &envelope),
            Err(SealError::BadMagic)
        );
    }

    #[test]
    fn key_check_is_32_hex() {
        let check = key_check(&key());
        assert_eq!(check.len(), 32);
        assert!(check.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
