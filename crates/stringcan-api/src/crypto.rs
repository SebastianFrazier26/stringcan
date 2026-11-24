//! Envelope for secrets stringcan must keep (the Plaid access token, DESIGN 20.4).
//! XChaCha20-Poly1305: its 24-byte nonce is safe to draw at random per message,
//! which AES-GCM's 12-byte nonce is not at volume. The tenant id is associated
//! data, so a ciphertext copied onto another tenant's row fails to decrypt.

use std::collections::HashMap;
use std::fmt;

use chacha20poly1305::aead::{Aead, Generate, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use uuid::Uuid;

const NONCE_LEN: usize = 24;
const AAD_PREFIX: &[u8] = b"stringcan/plaid-access-token/v1/";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("malformed key configuration: {0}")]
    Config(&'static str),
    #[error("ciphertext was encrypted with unknown key id")]
    UnknownKey,
    #[error("ciphertext failed authentication")]
    Inauthentic,
}

pub struct Sealed {
    pub key_id: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// One current key for encryption plus retired keys kept only to decrypt rows
/// not yet re-encrypted. Rotation: make the new key current, move the old one
/// to the retired list, and rows are re-sealed under the new key when next read.
pub struct Keyring {
    current: String,
    keys: HashMap<String, XChaCha20Poly1305>,
}

fn parse_entry(entry: &str) -> Result<(String, XChaCha20Poly1305), CryptoError> {
    let (id, hex_key) = entry
        .split_once(':')
        .ok_or(CryptoError::Config("expected <key id>:<64 hex chars>"))?;
    if id.is_empty() || id.len() > 16 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(CryptoError::Config(
            "key id must be 1 to 16 letters or digits",
        ));
    }
    let mut bytes =
        hex::decode(hex_key.trim()).map_err(|_| CryptoError::Config("key is not hex"))?;
    if bytes.len() != 32 {
        return Err(CryptoError::Config("key must be 32 bytes (64 hex chars)"));
    }
    // The .env.example dummy is all zeros; refusing it stops a copied example
    // file from becoming a real deployment's key.
    if bytes.iter().all(|&b| b == 0) {
        return Err(CryptoError::Config("all-zero key refused"));
    }
    let key = Key::try_from(bytes.as_slice()).map_err(|_| CryptoError::Config("key length"))?;
    bytes.fill(0);
    Ok((id.to_owned(), XChaCha20Poly1305::new(&key)))
}

fn aad(tenant: Uuid, key_id: &str) -> Vec<u8> {
    let mut v = AAD_PREFIX.to_vec();
    v.extend_from_slice(tenant.as_bytes());
    v.push(b'/');
    v.extend_from_slice(key_id.as_bytes());
    v
}

impl Keyring {
    /// `current` is `<id>:<hex>`; `retired` is a comma-separated list of the same.
    pub fn new(current: &str, retired: &str) -> Result<Self, CryptoError> {
        let (id, cipher) = parse_entry(current)?;
        let mut keys = HashMap::from([(id.clone(), cipher)]);
        for entry in retired.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (rid, rc) = parse_entry(entry)?;
            if keys.insert(rid, rc).is_some() {
                return Err(CryptoError::Config("duplicate key id"));
            }
        }
        Ok(Keyring { current: id, keys })
    }

    pub fn from_env() -> Result<Self, CryptoError> {
        let current = std::env::var("STRINGCAN_TOKEN_KEY")
            .map_err(|_| CryptoError::Config("STRINGCAN_TOKEN_KEY is not set"))?;
        let retired = std::env::var("STRINGCAN_TOKEN_KEYS_RETIRED").unwrap_or_default();
        Self::new(&current, &retired)
    }

    pub fn current_id(&self) -> &str {
        &self.current
    }

    pub fn seal(&self, tenant: Uuid, plaintext: &str) -> Sealed {
        let cipher = &self.keys[&self.current];
        let nonce = XNonce::generate();
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: &aad(tenant, &self.current),
                },
            )
            .expect("XChaCha20-Poly1305 encryption of a short message cannot fail");
        Sealed {
            key_id: self.current.clone(),
            nonce: nonce.to_vec(),
            ciphertext,
        }
    }

    pub fn open(&self, tenant: Uuid, sealed: &Sealed) -> Result<String, CryptoError> {
        let cipher = self
            .keys
            .get(&sealed.key_id)
            .ok_or(CryptoError::UnknownKey)?;
        if sealed.nonce.len() != NONCE_LEN {
            return Err(CryptoError::Inauthentic);
        }
        let nonce =
            XNonce::try_from(sealed.nonce.as_slice()).map_err(|_| CryptoError::Inauthentic)?;
        let plain = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &sealed.ciphertext,
                    aad: &aad(tenant, &sealed.key_id),
                },
            )
            .map_err(|_| CryptoError::Inauthentic)?;
        String::from_utf8(plain).map_err(|_| CryptoError::Inauthentic)
    }
}

impl fmt::Debug for Keyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut ids: Vec<&String> = self.keys.keys().collect();
        ids.sort();
        f.debug_struct("Keyring")
            .field("current", &self.current)
            .field("key_ids", &ids)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &str = "k1:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const K2: &str = "k2:1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100";

    #[test]
    fn round_trips_with_fresh_nonces() {
        let ring = Keyring::new(K1, "").unwrap();
        let t = Uuid::from_u128(rand::random());
        let a = ring.seal(t, "access-sandbox-abc");
        let b = ring.seal(t, "access-sandbox-abc");
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
        assert_eq!(a.key_id, "k1");
        assert_eq!(ring.open(t, &a).unwrap(), "access-sandbox-abc");
    }

    #[test]
    fn another_tenants_row_or_a_tampered_row_does_not_open() {
        let ring = Keyring::new(K1, "").unwrap();
        let t = Uuid::from_u128(rand::random());
        let mut s = ring.seal(t, "access-sandbox-abc");
        assert_eq!(
            ring.open(Uuid::from_u128(rand::random()), &s),
            Err(CryptoError::Inauthentic)
        );
        s.ciphertext[0] ^= 1;
        assert_eq!(ring.open(t, &s), Err(CryptoError::Inauthentic));
    }

    #[test]
    fn retired_keys_still_open_and_unknown_ids_are_refused() {
        let old = Keyring::new(K1, "").unwrap();
        let t = Uuid::from_u128(rand::random());
        let sealed = old.seal(t, "tok");
        let rotated = Keyring::new(K2, K1).unwrap();
        assert_eq!(rotated.open(t, &sealed).unwrap(), "tok");
        assert_eq!(rotated.seal(t, "tok").key_id, "k2");
        let mut relabelled = old.seal(t, "tok");
        relabelled.key_id = "k2".into();
        assert_eq!(rotated.open(t, &relabelled), Err(CryptoError::Inauthentic));
        let fresh = Keyring::new(K2, "").unwrap();
        assert_eq!(fresh.open(t, &sealed), Err(CryptoError::UnknownKey));
    }

    #[test]
    fn bad_configuration_is_rejected_and_debug_hides_keys() {
        let zero = format!("k0:{}", "0".repeat(64));
        for bad in [
            "nocolon",
            "k1:zz",
            "k1:0011",
            ":00",
            "k-1:00",
            zero.as_str(),
        ] {
            assert!(Keyring::new(bad, "").is_err(), "{bad}");
        }
        assert!(Keyring::new(K1, K1).is_err());
        let dbg = format!("{:?}", Keyring::new(K1, K2).unwrap());
        assert!(dbg.contains("k1") && !dbg.contains("000102"));
    }
}
