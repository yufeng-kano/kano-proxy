//! CLI device tokens (apps/api/src/auth/cli_tokens.ts): access token =
//! `base64url(JSON claims).hexHMAC(CLI_TOKEN_SECRET)` with a 1-hour `exp`; refresh token
//! `kpr_` + base64url(32 bytes), stored hashed; pairing code `XXXX-XXXX` from a 30-letter
//! alphabet.

use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use super::{base64url_no_pad, hmac_sha256_hex, sha256_hex, timing_safe_equal};

pub const ACCESS_TOKEN_TTL_S: i64 = 3600;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTVWXYZ23456789";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliTokenClaims {
    pub user_id: String,
    pub device_id: String,
    pub exp: i64,
}

pub struct MintedToken {
    pub token: String,
    pub expires_in: i64,
}

/// `now_ms` is Unix milliseconds, as `Date.now()`.
pub fn mint_access_token(secret: &str, user_id: &str, device_id: &str, now_ms: i64) -> MintedToken {
    let claims = CliTokenClaims {
        user_id: user_id.to_string(),
        device_id: device_id.to_string(),
        exp: now_ms.div_euclid(1000) + ACCESS_TOKEN_TTL_S,
    };
    let payload = base64url_no_pad(serde_json::to_string(&claims).expect("claims serialize").as_bytes());
    let sig = hmac_sha256_hex(secret, payload.as_bytes());
    MintedToken { token: format!("{payload}.{sig}"), expires_in: ACCESS_TOKEN_TTL_S }
}

pub fn verify_access_token(secret: &str, token: &str, now_ms: i64) -> Option<CliTokenClaims> {
    let dot = token.find('.')?;
    if dot == 0 {
        return None;
    }
    let (payload, sig) = (&token[..dot], &token[dot + 1..]);
    if payload.is_empty() || sig.is_empty() {
        return None;
    }
    if !timing_safe_equal(&hmac_sha256_hex(secret, payload.as_bytes()), sig) {
        return None;
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: CliTokenClaims = serde_json::from_slice(&decoded).ok()?;
    if claims.user_id.is_empty() || claims.device_id.is_empty() {
        return None;
    }
    if claims.exp.checked_mul(1000)? <= now_ms {
        return None;
    }
    Some(claims)
}

pub fn new_refresh_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("kpr_{}", base64url_no_pad(&bytes))
}

pub fn sha256_hex_str(value: &str) -> String {
    sha256_hex(value.as_bytes())
}

pub fn new_pairing_code() -> String {
    let mut bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut bytes);
    let chars: String = bytes.iter().map(|b| CODE_ALPHABET[*b as usize % CODE_ALPHABET.len()] as char).collect();
    format!("{}-{}", &chars[..4], &chars[4..])
}

pub fn normalize_pairing_code(code: &str) -> String {
    code.to_ascii_uppercase().chars().filter(|c| c.is_ascii_uppercase() || c.is_ascii_digit()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_then_verify() {
        let now = 1_757_900_000_000; // 2025-09-15T02:13:20Z
        let t = mint_access_token("s", "u1", "d1", now);
        assert_eq!(t.expires_in, 3600);
        let claims = verify_access_token("s", &t.token, now).unwrap();
        assert_eq!(claims, CliTokenClaims { user_id: "u1".into(), device_id: "d1".into(), exp: 1_757_900_000 + 3600 });
        assert!(verify_access_token("s", &t.token, now + 3600 * 1000).is_none());
        assert!(verify_access_token("other", &t.token, now).is_none());
        assert!(verify_access_token("s", "nodot", now).is_none());
        assert!(verify_access_token("s", ".sig", now).is_none());
    }

    #[test]
    fn upstream_payload_is_plain_json_base64url() {
        // The TypeScript side base64url-encodes `JSON.stringify(claims)` with keys in this
        // order; serde must emit the same bytes so signatures verify across implementations.
        let t = mint_access_token("s", "u", "d", 0);
        let payload = t.token.split('.').next().unwrap();
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).unwrap();
        assert_eq!(json, br#"{"user_id":"u","device_id":"d","exp":3600}"#);
    }

    #[test]
    fn verifies_token_minted_by_typescript() {
        let token = "eyJ1c2VyX2lkIjoidSIsImRldmljZV9pZCI6ImQiLCJleHAiOjM2MDB9.dae4ca150dbf13ace0f2e68ca2b843d8235586895835a9ff43384d494bf9a75d";
        let claims = verify_access_token("s", token, 0).unwrap();
        assert_eq!(claims.exp, 3600);
        assert_eq!(mint_access_token("s", "u", "d", 0).token, token);
    }

    #[test]
    fn pairing_code_shape() {
        let c = new_pairing_code();
        assert_eq!(c.len(), 9);
        assert_eq!(&c[4..5], "-");
        assert!(c.bytes().filter(|b| *b != b'-').all(|b| CODE_ALPHABET.contains(&b)));
        assert_eq!(normalize_pairing_code("ab3d-e f9g"), "AB3DEF9G");
        assert!(new_refresh_token().starts_with("kpr_"));
    }
}
