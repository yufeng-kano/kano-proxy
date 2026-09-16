//! Reasoning effort for BYO OpenAI providers (docs/providers.md
//! § Custom providers, "same-account remap, not a first-send clamp").
//!
//! An ordered registry of parsers that recognize "this valid ladder token is not in the
//! upstream's allowed set" in a 400 body. A new vendor error string is a new parser, not a
//! policy change.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};

use crate::utils::reasoning::{
    nearest_reasoning_effort, parse_reasoning_effort, parse_reasoning_effort_str, ParsedReasoningEffort,
    ReasoningEffort,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortRejection {
    pub rejected: ReasoningEffort,
    pub allowed: Vec<ReasoningEffort>,
}

/// `EffortRejectionParser` — a body text recognizer.
pub type EffortRejectionParser = fn(&str) -> Option<EffortRejection>;

/// `/unexpected reasoning effort\s+(\S+?)\.\s*supported types are\s+(.+)/i`.
static TABBY_QWEN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)unexpected reasoning effort\s+(\S+?)\.\s*supported types are\s+(.+)")
        .expect("tabby template error regex")
});

/// `/\(\s*default\s*\)/gi`.
static DEFAULT_MARKER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\(\s*default\s*\)").expect("default marker regex"));

/// `/,|\band\b/i`.
static SUPPORTED_SPLIT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i),|\band\b").expect("supported list split regex"));

/// Tabby + Qwen 3.8 Jinja:
/// `TemplateError: Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low.`
/// Matches anywhere so FastAPI `{"detail":"…"}` and a bare exception string both work.
pub fn parse_tabby_qwen_template_error(body_text: &str) -> Option<EffortRejection> {
    let caps = TABBY_QWEN_RE.captures(body_text)?;
    let rejected = parse_effort_token(caps.get(1)?.as_str())?;
    let allowed = parse_supported_effort_list(caps.get(2)?.as_str());
    if allowed.is_empty() {
        return None;
    }
    Some(EffortRejection { rejected, allowed })
}

const PARSERS: &[EffortRejectionParser] = &[parse_tabby_qwen_template_error];

pub fn parse_unsupported_effort_rejection(body_text: &str) -> Option<EffortRejection> {
    PARSERS.iter().find_map(|parse| parse(body_text))
}

/// Rewrite only `reasoning_effort` when a parser + nearest-neighbor say to retry.
pub fn remap_unsupported_effort_body(
    sent_body: &Map<String, Value>,
    body_text: &str,
) -> Option<Map<String, Value>> {
    let sent = match parse_reasoning_effort(sent_body.get("reasoning_effort")) {
        ParsedReasoningEffort::Effort(effort) => effort,
        // Absent or invalid: nothing to remap.
        _ => return None,
    };

    let parsed = parse_unsupported_effort_rejection(body_text)?;
    if parsed.allowed.contains(&parsed.rejected) {
        return None;
    }

    let mapped = nearest_reasoning_effort(parsed.rejected, &parsed.allowed)?;
    if mapped == sent {
        return None;
    }

    let mut out = sent_body.clone();
    out.insert("reasoning_effort".into(), Value::String(mapped.as_str().to_string()));
    Some(out)
}

fn parse_supported_effort_list(raw: &str) -> Vec<ReasoningEffort> {
    let cleaned = DEFAULT_MARKER_RE.replace_all(raw, " ");
    SUPPORTED_SPLIT_RE.split(&cleaned).filter_map(parse_effort_token).collect()
}

/// `raw.trim().replace(/^[^a-z0-9]+|[^a-z0-9]+$/gi, "")`, then the ladder parse.
fn parse_effort_token(raw: &str) -> Option<ReasoningEffort> {
    let trimmed = raw.trim().trim_matches(|c: char| !c.is_ascii_alphanumeric());
    match parse_reasoning_effort_str(trimmed) {
        ParsedReasoningEffort::Effort(effort) => Some(effort),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TABBY_DETAIL: &str =
        "TemplateError: Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low.";

    fn tabby_fastapi() -> String {
        json!({ "detail": TABBY_DETAIL }).to_string()
    }

    fn rejection(rejected: ReasoningEffort, allowed: &[ReasoningEffort]) -> EffortRejection {
        EffortRejection { rejected, allowed: allowed.to_vec() }
    }

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().expect("object").clone()
    }

    #[test]
    fn parses_the_fastapi_tabby_payload_in_document_order() {
        assert_eq!(
            parse_tabby_qwen_template_error(&tabby_fastapi()),
            Some(rejection(
                ReasoningEffort::High,
                &[ReasoningEffort::XHigh, ReasoningEffort::Medium, ReasoningEffort::Low]
            ))
        );
    }

    #[test]
    fn matches_a_bare_template_error_string() {
        assert_eq!(
            parse_tabby_qwen_template_error(TABBY_DETAIL),
            Some(rejection(
                ReasoningEffort::High,
                &[ReasoningEffort::XHigh, ReasoningEffort::Medium, ReasoningEffort::Low]
            ))
        );
    }

    #[test]
    fn strips_default_markers_from_the_supported_list() {
        assert_eq!(
            parse_tabby_qwen_template_error(
                "Unexpected reasoning effort medium. Supported types are low (default) and high."
            ),
            Some(rejection(ReasoningEffort::Medium, &[ReasoningEffort::Low, ReasoningEffort::High]))
        );
    }

    #[test]
    fn returns_none_for_unrecognized_400_text() {
        assert_eq!(parse_tabby_qwen_template_error(r#"{"error":"model not found"}"#), None);
        assert_eq!(parse_tabby_qwen_template_error("bad request"), None);
    }

    #[test]
    fn returns_none_for_a_garbage_rejected_token_or_empty_supported_list() {
        assert_eq!(
            parse_tabby_qwen_template_error(
                "TemplateError: Unexpected reasoning effort ultra. Supported types are xhigh, medium, and low."
            ),
            None
        );
        assert_eq!(
            parse_tabby_qwen_template_error("TemplateError: Unexpected reasoning effort high. Supported types are ."),
            None
        );
    }

    #[test]
    fn the_registry_returns_the_first_parsers_result() {
        assert_eq!(
            parse_unsupported_effort_rejection(&tabby_fastapi()),
            Some(rejection(
                ReasoningEffort::High,
                &[ReasoningEffort::XHigh, ReasoningEffort::Medium, ReasoningEffort::Low]
            ))
        );
    }

    #[test]
    fn the_registry_returns_none_when_no_parser_matches() {
        assert_eq!(parse_unsupported_effort_rejection("Internal Server Error"), None);
    }

    #[test]
    fn rewrites_only_reasoning_effort_to_the_nearest_allowed_token() {
        let sent = object(json!({ "model": "qwen", "messages": [], "reasoning_effort": "high", "temperature": 0.7 }));
        assert_eq!(
            remap_unsupported_effort_body(&sent, &tabby_fastapi()),
            Some(object(
                json!({ "model": "qwen", "messages": [], "reasoning_effort": "xhigh", "temperature": 0.7 })
            ))
        );
    }

    #[test]
    fn does_not_remap_when_no_reasoning_effort_was_sent() {
        assert_eq!(remap_unsupported_effort_body(&object(json!({ "model": "qwen" })), &tabby_fastapi()), None);
    }

    #[test]
    fn does_not_remap_an_unrecognized_400() {
        assert_eq!(
            remap_unsupported_effort_body(
                &object(json!({ "reasoning_effort": "high" })),
                r#"{"error":"context length exceeded"}"#
            ),
            None
        );
    }

    #[test]
    fn does_not_remap_when_the_rejected_token_is_already_allowed() {
        assert_eq!(
            remap_unsupported_effort_body(
                &object(json!({ "reasoning_effort": "medium" })),
                "Unexpected reasoning effort medium. Supported types are medium and high."
            ),
            None
        );
    }
}
