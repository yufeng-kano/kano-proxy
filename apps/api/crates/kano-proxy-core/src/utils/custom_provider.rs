//! Validation, masking, and limits for user-defined custom providers
//! (docs/providers.md § Custom providers).

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CustomProviderFormat {
    Openai,
    Anthropic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CustomProviderModelsMode {
    Auto,
    Manual,
}

impl CustomProviderFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            CustomProviderFormat::Openai => "openai",
            CustomProviderFormat::Anthropic => "anthropic",
        }
    }
    pub fn parse(v: &str) -> Option<Self> {
        match v {
            "openai" => Some(CustomProviderFormat::Openai),
            "anthropic" => Some(CustomProviderFormat::Anthropic),
            _ => None,
        }
    }
}

impl CustomProviderModelsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CustomProviderModelsMode::Auto => "auto",
            CustomProviderModelsMode::Manual => "manual",
        }
    }
    pub fn parse(v: &str) -> Option<Self> {
        match v {
            "auto" => Some(CustomProviderModelsMode::Auto),
            "manual" => Some(CustomProviderModelsMode::Manual),
            _ => None,
        }
    }
}

pub const MAX_CUSTOM_PROVIDERS_PER_USER: usize = 20;
pub const MAX_MANUAL_MODELS: usize = 100;
pub const MAX_MANUAL_MODEL_ID_LENGTH: usize = 128;

/// Names that would collide with a builtin provider or a reserved route segment.
pub const RESERVED_SLUGS: &[&str] = &[
    "claude-code",
    "codex",
    "grok",
    "antigravity",
    "openai",
    "anthropic",
    "claude",
    "gpt",
    "gemini",
    "google",
    "api",
    "admin",
    "custom",
    "models",
    "usage",
    "keys",
    "accounts",
    "kano",
    "kano-proxy",
    "group",
    "groups",
    "model-groups",
];

pub fn is_reserved_slug(slug: &str) -> bool {
    RESERVED_SLUGS.contains(&slug)
}

// Lowercase alphanumeric + hyphens, must start and end alphanumeric, 2-32 chars total.
static SLUG_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$").expect("slug regex"));

/// `v` comes off the wire, so a non-string is simply not a format (the TypeScript type guard).
pub fn is_custom_provider_format(v: &Value) -> bool {
    v.as_str().map(|s| CustomProviderFormat::parse(s).is_some()).unwrap_or(false)
}

pub fn is_models_mode(v: &Value) -> bool {
    v.as_str().map(|s| CustomProviderModelsMode::parse(s).is_some()).unwrap_or(false)
}

/// `None` is valid; `Some(message)` is the wire error, exactly as the TypeScript returns it.
pub fn validate_slug(slug: &str) -> Option<String> {
    let len = slug.chars().count();
    if !(2..=32).contains(&len) {
        return Some("slug must be 2-32 characters".into());
    }
    if !SLUG_RE.is_match(slug) {
        return Some(
            "slug must be lowercase alphanumeric with hyphens, starting and ending with a letter or digit"
                .into(),
        );
    }
    if is_reserved_slug(slug) {
        return Some(format!("slug \"{slug}\" is reserved"));
    }
    None
}

pub fn validate_name(name: &str) -> Option<String> {
    if name.is_empty() || name.chars().count() > 64 {
        return Some("name must be 1-64 characters".into());
    }
    None
}

pub fn validate_api_key(key: &str) -> Option<String> {
    if key.is_empty() || key.chars().count() > 512 {
        return Some("api_key must be 1-512 characters".into());
    }
    None
}

pub fn validate_base_url_length(url: &str, field_name: &str) -> Option<String> {
    if url.chars().count() > 300 {
        return Some(format!("{field_name} must be at most 300 characters"));
    }
    None
}

/// `None` (field omitted) means "no manual models" — not an error. A JSON `null` is a
/// non-array and is rejected, as in the TypeScript.
pub fn validate_manual_models(models: Option<&Value>) -> Result<Vec<String>, String> {
    let Some(models) = models else {
        return Ok(Vec::new());
    };
    let Some(arr) = models.as_array() else {
        return Err("manual_models must be an array".into());
    };
    if arr.len() > MAX_MANUAL_MODELS {
        return Err(format!("manual_models must have at most {MAX_MANUAL_MODELS} entries"));
    }
    let mut out = Vec::with_capacity(arr.len());
    for m in arr {
        let Some(s) = m.as_str() else {
            return Err("manual_models entries must be strings".into());
        };
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err("manual_models entries must not be empty".into());
        }
        if trimmed.chars().count() > MAX_MANUAL_MODEL_ID_LENGTH {
            return Err(format!(
                "manual_models entries must be at most {MAX_MANUAL_MODEL_ID_LENGTH} characters"
            ));
        }
        // "/" is allowed (upstream ids may be namespaced, e.g. "org/model"); only whitespace
        // is rejected.
        if trimmed.chars().any(char::is_whitespace) {
            return Err("manual_models entries must not contain whitespace".into());
        }
        out.push(trimmed.to_string());
    }
    Ok(out)
}

/// Tolerant read of the stored `manual_models_json` column.
pub fn parse_manual_models(json: Option<&str>) -> Vec<String> {
    let Some(json) = json.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    match serde_json::from_str::<Value>(json) {
        Ok(Value::Array(arr)) => {
            arr.into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
        }
        _ => Vec::new(),
    }
}

/// Non-secret display mask: first 6 + "…" + last 4 (e.g. "sk-abc…f3a2"). Keys shorter than
/// 12 characters mask everything but the last 2 characters.
pub fn mask_api_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let len = chars.len();
    if len >= 12 {
        let head: String = chars[..6].iter().collect();
        let tail: String = chars[len - 4..].iter().collect();
        return format!("{head}…{tail}");
    }
    let visible = len.min(2);
    let hidden = len - visible;
    let tail: String = chars[len - visible..].iter().collect();
    format!("{}{}", "*".repeat(hidden), tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn slug_accepts_plain_and_boundary_lengths() {
        assert_eq!(validate_slug("my-endpoint"), None);
        assert_eq!(validate_slug("ab"), None);
        assert_eq!(validate_slug(&"a".repeat(32)), None);
        assert_eq!(validate_slug("my-2nd-endpoint9"), None);
    }

    #[test]
    fn slug_rejects_bad_lengths_and_shapes() {
        for bad in ["a", "", &"a".repeat(33), "My-Endpoint", "-abc", "abc-", "ab c", "ab_c", "ab.c", "ab/c"] {
            assert!(validate_slug(bad).is_some(), "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn slug_rejects_every_reserved_name() {
        for slug in RESERVED_SLUGS {
            assert!(validate_slug(slug).is_some(), "{slug} must be reserved");
        }
    }

    #[test]
    fn reserved_list_covers_builtins_and_route_shaped_segments() {
        for s in ["claude-code", "codex", "grok", "api", "admin", "models", "kano-proxy"] {
            assert!(is_reserved_slug(s));
        }
    }

    #[test]
    fn name_and_api_key_bounds() {
        assert_eq!(validate_name("a"), None);
        assert_eq!(validate_name(&"a".repeat(64)), None);
        assert!(validate_name("").is_some());
        assert!(validate_name(&"a".repeat(65)).is_some());
        assert_eq!(validate_api_key("k"), None);
        assert_eq!(validate_api_key(&"k".repeat(512)), None);
        assert!(validate_api_key("").is_some());
        assert!(validate_api_key(&"k".repeat(513)).is_some());
    }

    #[test]
    fn base_url_length_bound() {
        assert_eq!(validate_base_url_length(&format!("https://example.com/{}", "a".repeat(279)), "base_url"), None);
        assert!(validate_base_url_length(&format!("https://example.com/{}", "a".repeat(400)), "base_url").is_some());
    }

    #[test]
    fn manual_models_omitted_is_an_empty_valid_list() {
        assert_eq!(validate_manual_models(None), Ok(Vec::new()));
    }

    #[test]
    fn manual_models_trims_and_allows_namespaced_ids() {
        assert_eq!(
            validate_manual_models(Some(&json!([" model-a ", "model-b"]))),
            Ok(vec!["model-a".to_string(), "model-b".to_string()])
        );
        assert_eq!(validate_manual_models(Some(&json!(["org/model"]))), Ok(vec!["org/model".to_string()]));
    }

    #[test]
    fn manual_models_rejects_bad_shapes_and_counts() {
        assert!(validate_manual_models(Some(&json!("model-a"))).is_err());
        let many: Vec<String> = (0..101).map(|i| format!("m{i}")).collect();
        assert!(validate_manual_models(Some(&json!(many))).is_err());
        let ok: Vec<String> = (0..100).map(|i| format!("m{i}")).collect();
        assert!(validate_manual_models(Some(&json!(ok))).is_ok());
        assert!(validate_manual_models(Some(&json!([123]))).is_err());
        assert!(validate_manual_models(Some(&json!(["   "]))).is_err());
        assert!(validate_manual_models(Some(&json!(["a".repeat(129)]))).is_err());
        assert!(validate_manual_models(Some(&json!(["a".repeat(128)]))).is_ok());
        assert!(validate_manual_models(Some(&json!(["model a"]))).is_err());
    }

    #[test]
    fn parse_manual_models_is_tolerant() {
        assert_eq!(parse_manual_models(None), Vec::<String>::new());
        assert_eq!(parse_manual_models(Some(r#"["a","b"]"#)), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(parse_manual_models(Some("not json")), Vec::<String>::new());
        assert_eq!(parse_manual_models(Some(r#"["a", 1, "b"]"#)), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn masks_a_long_key_as_head_ellipsis_tail() {
        assert_eq!(mask_api_key("sk-abcdefghijf3a2"), "sk-abc…f3a2");
        assert_eq!(mask_api_key("123456789012"), "123456…9012");
    }

    #[test]
    fn masks_short_keys_down_to_their_last_two_characters() {
        assert_eq!(mask_api_key("shortkey"), "******ey");
        assert_eq!(mask_api_key("ab"), "ab");
        assert_eq!(mask_api_key("a"), "a");
        assert_eq!(mask_api_key(""), "");
    }

    #[test]
    fn a_long_key_body_never_survives_the_mask() {
        let key = "sk-verysecretlongapikeyvalue1234";
        let masked = mask_api_key(key);
        assert!(!masked.contains(&key[6..key.len() - 4]));
    }

    #[test]
    fn format_and_mode_guards_accept_exactly_their_values() {
        assert!(is_custom_provider_format(&json!("openai")));
        assert!(is_custom_provider_format(&json!("anthropic")));
        assert!(!is_custom_provider_format(&json!("openrouter")));
        assert!(!is_custom_provider_format(&Value::Null));
        assert!(is_models_mode(&json!("auto")));
        assert!(is_models_mode(&json!("manual")));
        assert!(!is_models_mode(&json!("live")));
    }

    #[test]
    fn max_custom_providers_per_user_is_20() {
        assert_eq!(MAX_CUSTOM_PROVIDERS_PER_USER, 20);
    }
}
