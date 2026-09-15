//! Upstream credential encryption (apps/api/src/crypto/token_crypto.ts): AES-256-GCM,
//! 12-byte random IV, `base64(iv || ciphertext || tag)` of the JSON plaintext. Key
//! derivation: base64-decode `TOKEN_ENCRYPTION_KEY`; undecodable input is SHA-256'd,
//! and any length other than 32 bytes is SHA-256'd again. Existing rows depend on this.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use rand::RngCore;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("TOKEN_ENCRYPTION_KEY is not configured")]
    NotConfigured,
    #[error("ciphertext is not valid base64")]
    Base64,
    #[error("ciphertext is too short")]
    TooShort,
    #[error("decryption failed")]
    Decrypt,
    #[error("plaintext is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

fn derive_key(encryption_key_b64: Option<&str>) -> Result<[u8; 32], CryptoError> {
    let raw = encryption_key_b64.filter(|s| !s.is_empty()).ok_or(CryptoError::NotConfigured)?;
    // `atob` semantics: standard alphabet, padding optional, whitespace tolerated.
    let mut bytes = match decode_atob(raw) {
        Some(b) => b,
        None => Sha256::digest(raw.as_bytes()).to_vec(),
    };
    if bytes.len() != 32 {
        bytes = Sha256::digest(&bytes).to_vec();
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// Mirrors the browser `atob` acceptance rules closely enough for key material: strips
/// ASCII whitespace, allows missing padding, rejects any other invalid character.
fn decode_atob(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let trimmed = cleaned.trim_end_matches('=');
    if trimmed.len() % 4 == 1 {
        return None;
    }
    base64::engine::general_purpose::STANDARD_NO_PAD.decode(trimmed).ok()
}

pub fn encrypt_json<T: Serialize>(encryption_key_b64: Option<&str>, value: &T) -> Result<String, CryptoError> {
    let key = derive_key(encryption_key_b64)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut iv = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut iv);
    let plaintext = serde_json::to_vec(value)?;
    let ct = cipher.encrypt(Nonce::from_slice(&iv), plaintext.as_ref()).map_err(|_| CryptoError::Decrypt)?;
    let mut packed = Vec::with_capacity(12 + ct.len());
    packed.extend_from_slice(&iv);
    packed.extend_from_slice(&ct);
    Ok(base64::engine::general_purpose::STANDARD.encode(packed))
}

pub fn decrypt_json<T: DeserializeOwned>(encryption_key_b64: Option<&str>, blob: &str) -> Result<T, CryptoError> {
    let key = derive_key(encryption_key_b64)?;
    let raw = decode_atob(blob).ok_or(CryptoError::Base64)?;
    if raw.len() < 12 + 16 {
        return Err(CryptoError::TooShort);
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let plain = cipher
        .decrypt(Nonce::from_slice(&raw[..12]), &raw[12..])
        .map_err(|_| CryptoError::Decrypt)?;
    Ok(serde_json::from_slice(&plain)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Produced with Node's WebCrypto running the TypeScript algorithm verbatim
    // (see docs/rust-server.md § Compatibility fixtures). Key: base64 of 32 bytes 0x00..0x1f.
    const KEY_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
    const FIXTURE_JS: &str = "AAAAAAAAAAAAAAAAdZ7UvdZJ8M5X3MZefUKzo/AiNDAKrUJBoqSC/oI5MjMOgdW2kuv0ieEqYjo=";

    #[test]
    fn decrypts_webcrypto_fixture() {
        let v: serde_json::Value = decrypt_json(Some(KEY_B64), FIXTURE_JS).unwrap();
        assert_eq!(v, serde_json::json!({"access_token":"abc","n":1}));
    }

    #[test]
    fn round_trip_and_layout() {
        let blob = encrypt_json(Some(KEY_B64), &serde_json::json!({"k":"v"})).unwrap();
        let raw = base64::engine::general_purpose::STANDARD.decode(&blob).unwrap();
        assert_eq!(raw.len(), 12 + r#"{"k":"v"}"#.len() + 16);
        let v: serde_json::Value = decrypt_json(Some(KEY_B64), &blob).unwrap();
        assert_eq!(v["k"], "v");
    }

    #[test]
    fn key_derivation_fallbacks_match_upstream() {
        // Non-base64 → SHA-256(text); base64 of wrong length → SHA-256(bytes).
        let k1 = derive_key(Some("not base64!!")).unwrap();
        assert_eq!(k1.to_vec(), Sha256::digest(b"not base64!!").to_vec());
        let k2 = derive_key(Some("YWJj")).unwrap(); // "abc"
        assert_eq!(k2.to_vec(), Sha256::digest(b"abc").to_vec());
        assert!(matches!(derive_key(None), Err(CryptoError::NotConfigured)));
        assert!(matches!(derive_key(Some("")), Err(CryptoError::NotConfigured)));
    }

    #[test]
    fn wrong_key_fails() {
        assert!(matches!(
            decrypt_json::<serde_json::Value>(Some("other"), FIXTURE_JS),
            Err(CryptoError::Decrypt)
        ));
    }
}
