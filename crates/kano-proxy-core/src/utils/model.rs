//! Model id parsing (apps/api/src/utils/model.ts, docs/api.md § Model ids): everything before
//! the FIRST `/` is the provider (a builtin id or a custom slug), the rest is the upstream id
//! verbatim.

use crate::providers::ProviderId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModel {
    pub provider: ProviderId,
    pub upstream_model: String,
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitModelId {
    /// Text before the first "/" — a builtin provider id or a candidate custom slug.
    pub prefix: String,
    pub upstream_model: String,
    pub raw: String,
}

/// Split on the FIRST "/" only — upstream ids may legitimately contain further slashes (e.g.
/// a custom slug fronting `org/model`), so the rest of the string after the first separator
/// is the upstream id verbatim.
pub fn split_model_id(model: &str) -> Option<SplitModelId> {
    let raw = model.trim();
    let slash = raw.find('/')?;
    if slash == 0 || slash == raw.len() - 1 {
        return None;
    }
    let prefix = &raw[..slash];
    let upstream_model = &raw[slash + 1..];
    if upstream_model.is_empty() {
        return None;
    }
    Some(SplitModelId {
        prefix: prefix.to_string(),
        upstream_model: upstream_model.to_string(),
        raw: raw.to_string(),
    })
}

pub fn parse_model_id(model: &str) -> Option<ParsedModel> {
    let split = split_model_id(model)?;
    let provider = ProviderId::parse(&split.prefix)?;
    Some(ParsedModel { provider, upstream_model: split.upstream_model, raw: split.raw })
}

/// The Anthropic surface uses the same provider/model ids as OpenAI. Bare upstream ids (no
/// provider prefix) are rejected.
pub fn parse_anthropic_model(model: &str) -> Option<ParsedModel> {
    parse_model_id(model)
}

/// Best-effort provider extraction from a raw, possibly-malformed model string, for
/// `invalid_model` pre-dispatch logging only — never used for routing (that stays on
/// [`split_model_id`]'s stricter shape check). Text before the first "/", the whole string
/// when there is no "/" at all, or `"unknown"` when that is empty (leading "/", or an empty
/// model string).
pub fn logging_provider_from_raw_model(raw: &str) -> String {
    let prefix = raw.split('/').next().unwrap_or("");
    if prefix.is_empty() {
        "unknown".to_string()
    } else {
        prefix.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(prefix: &str, upstream: &str, raw: &str) -> Option<SplitModelId> {
        Some(SplitModelId { prefix: prefix.into(), upstream_model: upstream.into(), raw: raw.into() })
    }

    #[test]
    fn parses_provider_slash_model() {
        assert_eq!(
            parse_model_id("claude-code/claude-opus-5"),
            Some(ParsedModel {
                provider: ProviderId::ClaudeCode,
                upstream_model: "claude-opus-5".into(),
                raw: "claude-code/claude-opus-5".into(),
            })
        );
    }

    #[test]
    fn rejects_a_bare_model_and_an_unknown_provider() {
        assert_eq!(parse_model_id("claude-opus-5"), None);
        assert_eq!(parse_model_id("openai/gpt-4"), None);
    }

    #[test]
    fn anthropic_surface_requires_provider_slash_model() {
        assert_eq!(
            parse_anthropic_model("grok/grok-4.5"),
            Some(ParsedModel {
                provider: ProviderId::Grok,
                upstream_model: "grok-4.5".into(),
                raw: "grok/grok-4.5".into(),
            })
        );
        assert_eq!(parse_anthropic_model("claude-opus-5"), None);
    }

    #[test]
    fn splits_on_the_first_slash_only() {
        assert_eq!(
            split_model_id("claude-code/claude-opus-5"),
            split("claude-code", "claude-opus-5", "claude-code/claude-opus-5")
        );
        assert_eq!(
            split_model_id("my-endpoint/org/model-name"),
            split("my-endpoint", "org/model-name", "my-endpoint/org/model-name")
        );
    }

    #[test]
    fn does_not_require_a_known_provider_prefix() {
        assert_eq!(split_model_id("some-custom-slug/model"), split("some-custom-slug", "model", "some-custom-slug/model"));
    }

    #[test]
    fn rejects_bare_trailing_and_leading_slash_shapes() {
        assert_eq!(split_model_id("model"), None);
        assert_eq!(split_model_id("slug/"), None);
        assert_eq!(split_model_id("/model"), None);
    }

    #[test]
    fn trims_surrounding_whitespace_before_splitting() {
        assert_eq!(split_model_id("  slug/model  "), split("slug", "model", "slug/model"));
    }

    #[test]
    fn logging_provider_falls_back_to_unknown() {
        assert_eq!(logging_provider_from_raw_model("grok/grok-4.5"), "grok");
        assert_eq!(logging_provider_from_raw_model("bare-model"), "bare-model");
        assert_eq!(logging_provider_from_raw_model("/model"), "unknown");
        assert_eq!(logging_provider_from_raw_model(""), "unknown");
    }
}
