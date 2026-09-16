//! PKCE S256 helpers for provider OAuth. Verifier and state are
//! base64url without padding, the shape every provider's authorize URL expects.

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkcePair {
    pub code_verifier: String,
    pub code_challenge: String,
}

fn base64_url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

pub fn build_pkce_pair() -> PkcePair {
    let code_verifier = base64_url(&random_bytes(48));
    let code_challenge = base64_url(&Sha256::digest(code_verifier.as_bytes()));
    PkcePair { code_verifier, code_challenge }
}

pub fn build_state_token() -> String {
    base64_url(&random_bytes(24))
}

/// Parses an Anthropic-style `code#state` paste. The error text is shown to the operator.
pub fn parse_code_hash_state(value: &str) -> Result<(String, String), String> {
    let cleaned = value.trim();
    match cleaned.find('#') {
        Some(hash) if hash > 0 && hash != cleaned.len() - 1 => {
            Ok((cleaned[..hash].trim().to_string(), cleaned[hash + 1..].trim().to_string()))
        }
        _ => Err("Paste authorization as code#state from the callback page".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_is_s256_compatible() {
        let pair = build_pkce_pair();
        assert!(pair.code_verifier.len() > 20);
        assert!(pair.code_challenge.len() > 20);
        for c in ['+', '/', '='] {
            assert!(!pair.code_challenge.contains(c), "base64url only");
            assert!(!pair.code_verifier.contains(c));
        }
        // The challenge really is base64url(SHA-256(verifier)), not another random value.
        assert_eq!(pair.code_challenge, base64_url(&Sha256::digest(pair.code_verifier.as_bytes())));
        assert_ne!(build_pkce_pair().code_verifier, pair.code_verifier);
        assert_ne!(build_state_token(), build_state_token());
    }

    #[test]
    fn code_hash_state_parsing() {
        assert_eq!(parse_code_hash_state("abc#def").unwrap(), ("abc".into(), "def".into()));
        assert_eq!(parse_code_hash_state("  abc # def  ").unwrap(), ("abc".into(), "def".into()));
        // A missing, leading or trailing '#' carries no state to bind against.
        assert!(parse_code_hash_state("abc").is_err());
        assert!(parse_code_hash_state("#def").is_err());
        assert!(parse_code_hash_state("abc#").is_err());
    }
}
