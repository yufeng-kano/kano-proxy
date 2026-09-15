//! Byte-for-byte compatible with the Worker's WebCrypto code paths (docs/auth.md,
//! docs/rust-server.md § Compatibility): existing API keys, session cookies, encrypted
//! upstream credentials and CLI device tokens keep working after cutover.

pub mod cli_tokens;
pub mod keys;
pub mod session;
pub mod token_crypto;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn hmac_sha256_hex(secret: &str, data: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

/// Constant-time string comparison (length mismatch returns false immediately, as upstream).
pub fn timing_safe_equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.as_bytes().ct_eq(b.as_bytes()).into()
}

pub(crate) fn base64url_no_pad(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
