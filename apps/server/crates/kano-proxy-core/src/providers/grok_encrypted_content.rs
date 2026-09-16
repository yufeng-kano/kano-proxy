//! Grok encrypted content (docs/providers.md § Grok).
//!
//! Transport-shape check for xAI/Grok `reasoning.encrypted_content`.
//!
//! Does not prove decryptability. Rejects known Claude/GPT/Gemini envelopes and
//! low-entropy blobs so foreign Anthropic `thinking.signature` values are never
//! forwarded as Grok ciphertext. Inspired by CLIProxyAPI InspectGrokEncryptedContent.

use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
use once_cell::sync::Lazy;

const MAX_LEN: usize = 8 * 1024 * 1024;
const MIN_DECODED_LEN: usize = 32;
const MIN_ENTROPY_RATIO: f64 = 0.85;

/// First chars that can begin a self-describing foreign envelope: Claude E/R/CAIS
/// plus GPT `gAAAA` — enough for a fast reject.
const SELF_DESCRIBING_FIRST: &[u8] = b"CERg";

/// `atob`-equivalent: standard alphabet, padding optional, trailing bits tolerated.
static B64: Lazy<GeneralPurpose> = Lazy::new(|| {
    let config = GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent);
    GeneralPurpose::new(&alphabet::STANDARD, config)
});

pub fn is_valid_grok_encrypted_content(raw: &str) -> bool {
    inspect_grok_encrypted_content(raw).is_none()
}

/// Returns an error reason, or `None` when the blob looks replay-safe for xAI.
pub fn inspect_grok_encrypted_content(raw: &str) -> Option<&'static str> {
    if raw.is_empty() {
        return Some("empty");
    }
    if raw != raw.trim() {
        return Some("whitespace");
    }
    if raw.chars().count() > MAX_LEN {
        return Some("too_long");
    }
    // Provider / GPT envelope rejects before the base64 alphabet scan — foreign
    // signatures often contain `#` or `-` that would otherwise report non_base64.
    if let Some(hash) = raw.find('#') {
        if hash > 0 && hash < 32 && is_provider_prefix(&raw.as_bytes()[..hash]) {
            return Some("provider_prefix");
        }
    }
    if raw.starts_with("gAAAA") {
        return Some("gpt_envelope");
    }
    if raw.contains('=') {
        return Some("padded_base64");
    }
    for &c in raw.as_bytes() {
        if c >= 128 || !is_base64_std(c) {
            return Some("non_base64");
        }
    }
    let first = raw.as_bytes()[0];
    if SELF_DESCRIBING_FIRST.contains(&first) {
        if looks_like_claude_thinking_signature(raw.as_bytes()) {
            return Some("claude_thinking");
        }
        if looks_like_claude_cais_signature(raw.as_bytes()) {
            return Some("claude_cais");
        }
    }

    let decoded = match decode_raw_std_base64(raw.as_bytes()) {
        Some(d) => d,
        None => return Some("base64_decode"),
    };
    if decoded.len() < MIN_DECODED_LEN {
        return Some("too_short");
    }
    if byte_entropy_ratio(&decoded) < MIN_ENTROPY_RATIO {
        return Some("low_entropy");
    }
    None
}

fn is_provider_prefix(prefix: &[u8]) -> bool {
    !prefix.is_empty() && prefix.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
}

fn is_base64_std(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'+' || c == b'/'
}

/// Classic Claude thinking envelope (CPA Strict spirit): after base64 decode the
/// payload must start with protobuf magic `0x12`. An E/R first character alone is
/// NOT enough — ~1/64 of uniform Grok ciphertext is E-prefixed and must still be
/// accepted.
fn looks_like_claude_thinking_signature(sig: &[u8]) -> bool {
    let first = match sig.first() {
        Some(c) => *c,
        None => return false,
    };
    if first != b'E' && first != b'R' {
        return false;
    }
    if first == b'E' {
        return claude_single_layer_magic_ok(sig);
    }
    // R-form: outer decode is ASCII E-form, which must itself carry 0x12 magic.
    let outer = match decode_raw_std_base64(sig) {
        Some(o) => o,
        None => return false,
    };
    if outer.first() != Some(&0x45) {
        return false;
    }
    // Inner E-form may still carry standard padding characters.
    let mut inner: &[u8] = &outer;
    while inner.last() == Some(&b'=') {
        inner = &inner[..inner.len() - 1];
    }
    claude_single_layer_magic_ok(inner)
}

fn claude_single_layer_magic_ok(e_form: &[u8]) -> bool {
    if e_form.first() != Some(&b'E') {
        return false;
    }
    // Magic byte 0x12 identifies the classic Claude thinking protobuf envelope.
    matches!(decode_raw_std_base64(e_form), Some(d) if d.first() == Some(&0x12))
}

fn looks_like_claude_cais_signature(sig: &[u8]) -> bool {
    // CAIS envelopes start with 'C'; decoded first byte 0x08; model text has "claude-".
    if sig.first() != Some(&b'C') {
        return false;
    }
    let decoded = match decode_raw_std_base64(sig) {
        Some(d) => d,
        None => return false,
    };
    if decoded.len() < 8 || decoded[0] != 0x08 {
        return false;
    }
    String::from_utf8_lossy(&decoded).contains("claude-")
}

fn decode_raw_std_base64(sig: &[u8]) -> Option<Vec<u8>> {
    // `atob` requires padding; restore it for decode only.
    let pad = (4 - sig.len() % 4) % 4;
    if pad == 3 {
        return None;
    }
    let mut padded = sig.to_vec();
    padded.extend(std::iter::repeat_n(b'=', pad));
    B64.decode(&padded).ok()
}

fn byte_entropy_ratio(buf: &[u8]) -> f64 {
    if buf.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    for &b in buf {
        counts[b as usize] += 1;
    }
    let n = buf.len() as f64;
    let mut entropy = 0.0f64;
    for c in counts {
        if c == 0 {
            continue;
        }
        let p = c as f64 / n;
        entropy -= p * p.log2();
    }
    let max_symbols = buf.len().min(256);
    if max_symbols <= 1 {
        return 0.0;
    }
    entropy / (max_symbols as f64).log2()
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;

    fn unpadded_b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes)
    }

    /// Deterministic high-entropy unpadded standard base64 (tests/helpers/grok_sig.ts).
    pub fn fake_grok_encrypted_content(seed: u32) -> String {
        for attempt in 0..32u32 {
            let mut bytes = [0u8; 48];
            for (i, b) in bytes.iter_mut().enumerate() {
                let i = i as u32;
                *b = (seed
                    .wrapping_add(attempt)
                    .wrapping_add(i)
                    .wrapping_mul(1103515245)
                    .wrapping_add(12345)
                    .wrapping_add(i.wrapping_mul(17))
                    & 0xff) as u8;
            }
            bytes[0] = 0x7f;
            bytes[1] = 0xa3;
            bytes[2] = 0x11;
            let b64 = unpadded_b64(&bytes);
            if is_valid_grok_encrypted_content(&b64) {
                return b64;
            }
        }
        panic!("failed to synthesize valid grok encrypted_content fixture");
    }

    /// High-entropy payload whose base64 starts with `E` but decoded[0] != 0x12.
    pub fn fake_e_prefixed_grok_ciphertext() -> String {
        let seed = fake_grok_encrypted_content(77);
        let mut bytes = decode_raw_std_base64(seed.as_bytes()).expect("fixture decodes");
        bytes[0] = 0x11; // 00010001 → base64 'E', not Claude magic 0x12
        let b64 = unpadded_b64(&bytes);
        assert!(b64.starts_with('E'), "expected E prefix, got {}", &b64[..1]);
        assert!(
            is_valid_grok_encrypted_content(&b64),
            "E-prefixed Grok fixture rejected: {:?}",
            inspect_grok_encrypted_content(&b64)
        );
        b64
    }

    /// Classic Claude E-form: decoded payload starts with protobuf magic 0x12.
    pub fn fake_claude_e_form_thinking_signature() -> String {
        let mut bytes = [0u8; 48];
        bytes[0] = 0x12;
        for (i, b) in bytes.iter_mut().enumerate().skip(1) {
            *b = ((i as u32 + 9).wrapping_mul(1664525).wrapping_add(1013904223) & 0xff) as u8;
        }
        let b64 = unpadded_b64(&bytes);
        assert!(b64.starts_with('E'), "expected Claude E-form to start with E");
        b64
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::*;
    use super::*;

    #[test]
    fn accepts_high_entropy_unpadded_base64() {
        assert!(is_valid_grok_encrypted_content(&fake_grok_encrypted_content(9)));
    }

    #[test]
    fn accepts_e_prefixed_grok_ciphertext_that_is_not_a_claude_envelope() {
        let sig = fake_e_prefixed_grok_ciphertext();
        assert!(sig.starts_with('E'));
        assert_eq!(inspect_grok_encrypted_content(&sig), None);
        assert!(is_valid_grok_encrypted_content(&sig));
    }

    #[test]
    fn rejects_gpt_envelopes() {
        assert_eq!(
            inspect_grok_encrypted_content("gAAAAABopenai-encrypted-content-blob"),
            Some("gpt_envelope")
        );
    }

    #[test]
    fn rejects_provider_cache_prefixes() {
        let body = fake_grok_encrypted_content(2);
        for provider in ["claude", "gemini", "openai"] {
            assert_eq!(
                inspect_grok_encrypted_content(&format!("{provider}#{body}")),
                Some("provider_prefix")
            );
        }
    }

    #[test]
    fn rejects_claude_e_form_thinking_signatures() {
        let e_form = fake_claude_e_form_thinking_signature();
        assert_eq!(inspect_grok_encrypted_content(&e_form), Some("claude_thinking"));
        assert!(!is_valid_grok_encrypted_content(&e_form));
    }

    #[test]
    fn rejects_empty_whitespace_and_padded_base64() {
        assert_eq!(inspect_grok_encrypted_content(""), Some("empty"));
        assert_eq!(inspect_grok_encrypted_content(" abc"), Some("whitespace"));
        assert_eq!(inspect_grok_encrypted_content("YWI="), Some("padded_base64"));
    }

    #[test]
    fn rejects_short_and_low_entropy_blobs() {
        assert_eq!(inspect_grok_encrypted_content("AAAA"), Some("too_short"));
        assert_eq!(inspect_grok_encrypted_content(&"A".repeat(64)), Some("low_entropy"));
    }
}
