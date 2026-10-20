//! Encrypted files (E2EE.md §7).
//!
//! Each file gets its own random 256-bit key and is encrypted with
//! XChaCha20-Poly1305 in the STREAM construction (Hoang, Reyhanitabar, Rogaway
//! and Vizár, "Online Authenticated-Encryption and its Nonce-Reuse
//! Misuse-Resistance"), in 64 KiB chunks, as the RustCrypto `aead::stream`
//! module implements it. STREAM authenticates each chunk's place and marks
//! the last one, so chunks can't be reordered, dropped or cut off unnoticed.
//!
//! The ciphertext is `DMF1`, a random 19-byte nonce prefix, then the chunks.
//! The key, and the SHA-256 of the ciphertext, travel inside the encrypted
//! post ([`crate::wire::FileKey`]).

use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::aead::KeyInit;
use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{Error, Result};

const MAGIC: &[u8; 4] = b"DMF1";
const NONCE: usize = 19;
const HEADER: usize = MAGIC.len() + NONCE;
/// Plaintext per chunk; each ciphertext chunk is this plus a 16-byte tag.
pub const CHUNK: usize = 64 * 1024;
const TAG: usize = 16;

/// A file's key.
pub struct FileSecret(Zeroizing<[u8; 32]>);

impl FileSecret {
    pub fn generate() -> Self {
        FileSecret(Zeroizing::new(crate::random::<32>()))
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let key: [u8; 32] = bytes.try_into().map_err(|_| Error::Malformed("file key"))?;
        Ok(FileSecret(Zeroizing::new(key)))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(self.0.as_ref().into())
    }
}

/// Encrypts a whole file with a fresh key.
pub fn encrypt(plain: &[u8]) -> (FileSecret, Vec<u8>) {
    let secret = FileSecret::generate();
    let nonce = crate::random::<NONCE>();
    let mut out = Vec::with_capacity(HEADER + plain.len() + (plain.len() / CHUNK + 1) * TAG);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    let mut enc = EncryptorBE32::from_aead(secret.cipher(), nonce.as_ref().into());
    // The last chunk is always written, empty if the file divides evenly, so
    // an empty file still has one.
    let mut chunks = plain.chunks(CHUNK).peekable();
    let mut last: &[u8] = &[];
    while let Some(chunk) = chunks.next() {
        if chunks.peek().is_none() {
            last = chunk;
            break;
        }
        out.extend(enc.encrypt_next(chunk).expect("a chunk within STREAM's limits"));
    }
    out.extend(enc.encrypt_last(last).expect("a chunk within STREAM's limits"));
    (secret, out)
}

/// Decrypts what [`encrypt`] made. Fails if any byte was changed, chunks
/// were moved, or the end was cut off.
pub fn decrypt(secret: &FileSecret, ciphertext: &[u8]) -> Result<Vec<u8>> {
    let bad = || Error::Untrusted("the file was changed or cut short".into());
    if ciphertext.len() < HEADER + TAG || &ciphertext[..MAGIC.len()] != MAGIC {
        return Err(Error::Malformed("encrypted file"));
    }
    let (nonce, body) = ciphertext[MAGIC.len()..].split_at(NONCE);
    let mut dec = DecryptorBE32::from_aead(secret.cipher(), nonce.into());
    let mut out = Vec::with_capacity(body.len());
    let mut chunks = body.chunks(CHUNK + TAG).peekable();
    while let Some(chunk) = chunks.next() {
        if chunks.peek().is_none() {
            out.extend(dec.decrypt_last(chunk).map_err(|_| bad())?);
            return Ok(out);
        }
        out.extend(dec.decrypt_next(chunk).map_err(|_| bad())?);
    }
    Err(bad())
}

/// SHA-256, hex: how a [`crate::wire::FileKey`] names its ciphertext.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_round_trip_at_every_size() {
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 17] {
            let plain: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let (key, ct) = encrypt(&plain);
            assert_eq!(ct.len(), HEADER + len + len.div_ceil(CHUNK).max(1) * TAG);
            assert_eq!(decrypt(&key, &ct).unwrap(), plain, "{len} bytes");
        }
    }

    #[test]
    fn tampering_is_caught() {
        let plain = vec![7u8; 2 * CHUNK + 5];
        let (key, ct) = encrypt(&plain);
        // Another key.
        assert!(decrypt(&FileSecret::generate(), &ct).is_err());
        // A flipped byte, anywhere.
        for at in [5, HEADER, HEADER + CHUNK + 3, ct.len() - 1] {
            let mut bad = ct.clone();
            bad[at] ^= 1;
            assert!(decrypt(&key, &bad).is_err(), "byte {at}");
        }
        // Cut off after a whole chunk: the cut chunk wasn't the last one.
        assert!(decrypt(&key, &ct[..HEADER + CHUNK + TAG]).is_err());
        // Chunks swapped.
        let mut swapped = ct[..HEADER].to_vec();
        swapped.extend_from_slice(&ct[HEADER + CHUNK + TAG..HEADER + 2 * (CHUNK + TAG)]);
        swapped.extend_from_slice(&ct[HEADER..HEADER + CHUNK + TAG]);
        swapped.extend_from_slice(&ct[HEADER + 2 * (CHUNK + TAG)..]);
        assert!(decrypt(&key, &swapped).is_err());
        assert!(decrypt(&key, b"DMF1").is_err());
    }

    #[test]
    fn hashes_are_hex() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
