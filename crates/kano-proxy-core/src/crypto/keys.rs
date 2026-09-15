//! Project API keys (apps/api/src/crypto/keys.ts): `sk-kano-proxy-` + base64url(24 random
//! bytes); stored as hex SHA-256 of the full plaintext plus a 20-character display prefix.

use rand::RngCore;

use super::{base64url_no_pad, sha256_hex};

pub const KEY_PREFIX: &str = "sk-kano-proxy-";
const DISPLAY_PREFIX_LENGTH: usize = 20;

pub struct ApiKeyMaterial {
    pub plaintext: String,
    pub prefix: String,
    pub hash: String,
}

pub fn hash_api_key(plaintext: &str) -> String {
    sha256_hex(plaintext.as_bytes())
}

pub fn create_api_key_material() -> ApiKeyMaterial {
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    let plaintext = format!("{KEY_PREFIX}{}", base64url_no_pad(&bytes));
    let hash = hash_api_key(&plaintext);
    let prefix = plaintext.chars().take(DISPLAY_PREFIX_LENGTH).collect();
    ApiKeyMaterial { plaintext, prefix, hash }
}

/// `Authorization: Bearer <token>` (case-insensitive scheme, trimmed token).
pub fn extract_bearer(auth_header: Option<&str>) -> Option<String> {
    let value = auth_header?;
    let (scheme, rest) = value.split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim();
    (!token.is_empty()).then(|| token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_webcrypto_sha256_hex() {
        // node: crypto.createHash('sha256').update('sk-kano-proxy-abc').digest('hex')
        assert_eq!(hash_api_key("sk-kano-proxy-abc"), "601dca61e60a9022668d87d4481cdae691b310ec1317b991133f5bbeddb12b0a");
        assert_eq!(super::super::sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn material_shape() {
        let m = create_api_key_material();
        assert!(m.plaintext.starts_with(KEY_PREFIX));
        assert_eq!(m.plaintext.len(), KEY_PREFIX.len() + 32);
        assert_eq!(m.prefix.len(), 20);
        assert_eq!(m.hash.len(), 64);
        assert!(!m.plaintext.contains(['+', '/', '=']));
    }

    #[test]
    fn bearer_extraction() {
        assert_eq!(extract_bearer(Some("Bearer  abc ")).as_deref(), Some("abc"));
        assert_eq!(extract_bearer(Some("bearer abc")).as_deref(), Some("abc"));
        assert_eq!(extract_bearer(Some("Basic abc")), None);
        assert_eq!(extract_bearer(Some("Bearer ")), None);
        assert_eq!(extract_bearer(None), None);
    }
}
