//! Ids, session tokens and the clock.

use sha2::{Digest, Sha256};

/// Mattermost's id alphabet: base32 without the letters that read alike.
const ALPHABET: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";

/// A new random 26-character id, the shape Mattermost clients expect.
pub fn new_id() -> String {
    encode(&rand::random::<[u8; 16]>())
}

/// A session token: 128 random bits, same shape as an id.
pub fn new_token() -> String {
    new_id()
}

/// What's stored for a token, so a leaked database holds no usable sessions.
pub fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Unix time in milliseconds.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 8 / 5 + 1);
    let (mut buf, mut bits) = (0u32, 0);
    for &b in bytes {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[(buf >> bits) as usize & 31] as char);
        }
        buf &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(ALPHABET[(buf << (5 - bits)) as usize & 31] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_look_like_mattermost_ids() {
        let id = new_id();
        assert_eq!(id.len(), 26);
        assert!(id.bytes().all(|b| ALPHABET.contains(&b)));
        assert_ne!(new_id(), id);
        assert_eq!(encode(&[0; 16]), "y".repeat(26));
    }

    #[test]
    fn token_hashes_are_hex_sha256() {
        assert_eq!(hash_token("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
