//! Reasoning effort ladder (apps/api/src/utils/reasoning.ts, docs/api.md § reasoning
//! effort). The ladder is `none < low < medium < high < xhigh < max`; every provider
//! declares the highest token its API accepts and efforts above it clamp down instead of
//! erroring. [`map_reasoning`] produces the provider-specific body fragment.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::providers::ProviderId;

/// Ladder order is the variant order: `REASONING_EFFORTS.indexOf` is [`ReasoningEffort::index`]
/// and the derived `Ord` is the ladder comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

/// The ladder in order, as the TypeScript `REASONING_EFFORTS`.
pub const REASONING_EFFORTS: [ReasoningEffort; 6] = [
    ReasoningEffort::None,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
    ReasoningEffort::Max,
];

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            ReasoningEffort::None => "none",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::XHigh => "xhigh",
            ReasoningEffort::Max => "max",
        }
    }

    /// Position on the ladder (`REASONING_EFFORTS.indexOf`).
    pub fn index(self) -> usize {
        REASONING_EFFORTS.iter().position(|e| *e == self).expect("ladder contains every variant")
    }

    /// Exact (already lowercased) ladder token, or `None` for anything else.
    pub fn from_token(token: &str) -> Option<ReasoningEffort> {
        REASONING_EFFORTS.iter().copied().find(|e| e.as_str() == token)
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `parseReasoningEffort`'s three outcomes: absent (`undefined`/`null`/`""`), a ladder
/// token, or `"invalid"` (a non-string, or a string that is not a ladder token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedReasoningEffort {
    Absent,
    Effort(ReasoningEffort),
    Invalid,
}

impl ParsedReasoningEffort {
    pub fn is_invalid(self) -> bool {
        matches!(self, ParsedReasoningEffort::Invalid)
    }
    /// The parsed effort, treating "absent" and "invalid" alike as no effort.
    pub fn effort(self) -> Option<ReasoningEffort> {
        match self {
            ParsedReasoningEffort::Effort(e) => Some(e),
            _ => None,
        }
    }
}

/// `parseReasoningEffort(v)` over a request field: `undefined`/`null`/`""` → absent,
/// a case-insensitive ladder token → that token, anything else → invalid.
pub fn parse_reasoning_effort(value: Option<&Value>) -> ParsedReasoningEffort {
    match value {
        None | Some(Value::Null) => ParsedReasoningEffort::Absent,
        Some(Value::String(s)) => {
            if s.is_empty() {
                return ParsedReasoningEffort::Absent;
            }
            match ReasoningEffort::from_token(&s.to_lowercase()) {
                Some(e) => ParsedReasoningEffort::Effort(e),
                None => ParsedReasoningEffort::Invalid,
            }
        }
        Some(_) => ParsedReasoningEffort::Invalid,
    }
}

/// `parseReasoningEffort` for a value that is already known to be a string.
pub fn parse_reasoning_effort_str(value: &str) -> ParsedReasoningEffort {
    parse_reasoning_effort(Some(&Value::String(value.to_string())))
}

/// Closest allowed ladder token to `rejected`. Equal distance prefers the higher token.
/// Empty `allowed` → `None`. Identity when `rejected` is already in `allowed`.
pub fn nearest_reasoning_effort(
    rejected: ReasoningEffort,
    allowed: &[ReasoningEffort],
) -> Option<ReasoningEffort> {
    if allowed.is_empty() {
        return None;
    }
    if allowed.contains(&rejected) {
        return Some(rejected);
    }
    let rejected_idx = rejected.index() as isize;
    let mut best: Option<ReasoningEffort> = None;
    let mut best_dist = usize::MAX;
    for token in allowed {
        let idx = token.index() as isize;
        let dist = (idx - rejected_idx).unsigned_abs();
        match best {
            None => {
                best = Some(*token);
                best_dist = dist;
            }
            Some(current) => {
                if dist < best_dist {
                    best = Some(*token);
                    best_dist = dist;
                } else if dist == best_dist && token.index() > current.index() {
                    best = Some(*token);
                }
            }
        }
    }
    best
}

/// Highest effort each provider's API accepts; efforts above it clamp down instead of
/// erroring. Verified 2026-08-02: xAI tops out at `xhigh` and codex Responses models top
/// out at `xhigh` (`max` is only a non-codex GPT-5.6 value). Gemini `thinkingLevel` tops
/// out at `high`. See docs/api.md.
pub fn reasoning_ceiling(provider: ProviderId) -> ReasoningEffort {
    match provider {
        ProviderId::Grok => ReasoningEffort::XHigh,
        ProviderId::Codex => ReasoningEffort::XHigh,
        ProviderId::ClaudeCode => ReasoningEffort::Max,
        ProviderId::Antigravity => ReasoningEffort::High,
    }
}

/// Clamp `effort` to the provider ceiling.
pub fn clamp_to_ceiling(provider: ProviderId, effort: ReasoningEffort) -> ReasoningEffort {
    let cap = reasoning_ceiling(provider);
    if effort.index() > cap.index() {
        cap
    } else {
        effort
    }
}

/// Map a client reasoning effort to the provider-specific payload fragment (key order as
/// the TypeScript object literals produce it). `None` effort → an empty object.
pub fn map_reasoning(provider: ProviderId, effort: Option<ReasoningEffort>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(effort) = effort else { return out };
    let capped = clamp_to_ceiling(provider, effort);

    match provider {
        ProviderId::Grok => {
            out.insert("reasoning_effort".into(), json!(capped.as_str()));
        }
        ProviderId::Codex => {
            if capped != ReasoningEffort::None {
                out.insert("reasoning".into(), json!({ "effort": capped.as_str(), "summary": "auto" }));
            }
        }
        ProviderId::Antigravity => {
            // Gemini's own off switch is a zero budget, not a level — `thinkingLevel`
            // has no "none" token.
            if capped == ReasoningEffort::None {
                out.insert("thinkingConfig".into(), json!({ "thinkingBudget": 0 }));
            } else {
                out.insert("thinkingConfig".into(), json!({ "thinkingLevel": capped.as_str() }));
            }
        }
        ProviderId::ClaudeCode => {
            // claude-code: effort-only public API; map none → disabled thinking + low effort.
            if capped == ReasoningEffort::None {
                out.insert("thinking".into(), json!({ "type": "disabled" }));
                out.insert("output_config".into(), json!({ "effort": "low" }));
            } else {
                out.insert("output_config".into(), json!({ "effort": capped.as_str() }));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: Value) -> ParsedReasoningEffort {
        parse_reasoning_effort(Some(&v))
    }
    fn map(provider: ProviderId, effort: Option<ReasoningEffort>) -> Value {
        Value::Object(map_reasoning(provider, effort))
    }

    #[test]
    fn parse_accepts_ladder() {
        use ReasoningEffort::*;
        for (token, expected) in [
            ("high", High),
            ("none", None),
            ("low", Low),
            ("medium", Medium),
            ("xhigh", XHigh),
            ("max", Max),
        ] {
            assert_eq!(parse(json!(token)), ParsedReasoningEffort::Effort(expected));
        }
    }

    #[test]
    fn parse_normalizes_case() {
        assert_eq!(parse(json!("HIGH")), ParsedReasoningEffort::Effort(ReasoningEffort::High));
        assert_eq!(parse(json!("XHigh")), ParsedReasoningEffort::Effort(ReasoningEffort::XHigh));
    }

    #[test]
    fn parse_treats_empty_as_absent() {
        assert_eq!(parse_reasoning_effort(Option::None), ParsedReasoningEffort::Absent);
        assert_eq!(parse(json!(null)), ParsedReasoningEffort::Absent);
        assert_eq!(parse(json!("")), ParsedReasoningEffort::Absent);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse(json!("ultra")), ParsedReasoningEffort::Invalid);
        assert_eq!(parse(json!(" ")), ParsedReasoningEffort::Invalid);
        assert_eq!(parse(json!(1)), ParsedReasoningEffort::Invalid);
        assert_eq!(parse(json!(true)), ParsedReasoningEffort::Invalid);
        assert_eq!(parse(json!({})), ParsedReasoningEffort::Invalid);
    }

    #[test]
    fn map_returns_empty_when_effort_is_absent() {
        for provider in [ProviderId::ClaudeCode, ProviderId::Codex, ProviderId::Grok] {
            assert_eq!(map(provider, Option::None), json!({}));
        }
    }

    #[test]
    fn map_claude_none_to_disabled_thinking() {
        assert_eq!(
            map(ProviderId::ClaudeCode, Some(ReasoningEffort::None)),
            json!({ "thinking": { "type": "disabled" }, "output_config": { "effort": "low" } })
        );
    }

    #[test]
    fn map_claude_high_to_output_config_only() {
        assert_eq!(
            map(ProviderId::ClaudeCode, Some(ReasoningEffort::High)),
            json!({ "output_config": { "effort": "high" } })
        );
    }

    #[test]
    fn map_claude_max_and_xhigh_to_output_config_effort() {
        assert_eq!(
            map(ProviderId::ClaudeCode, Some(ReasoningEffort::Max)),
            json!({ "output_config": { "effort": "max" } })
        );
        assert_eq!(
            map(ProviderId::ClaudeCode, Some(ReasoningEffort::XHigh)),
            json!({ "output_config": { "effort": "xhigh" } })
        );
    }

    #[test]
    fn map_codex_none_to_empty() {
        assert_eq!(map(ProviderId::Codex, Some(ReasoningEffort::None)), json!({}));
    }

    #[test]
    fn map_codex_efforts_to_reasoning_summary_auto() {
        for effort in [ReasoningEffort::Low, ReasoningEffort::Medium, ReasoningEffort::High] {
            assert_eq!(
                map(ProviderId::Codex, Some(effort)),
                json!({ "reasoning": { "effort": effort.as_str(), "summary": "auto" } })
            );
        }
    }

    #[test]
    fn map_clamps_codex_max_to_xhigh() {
        assert_eq!(
            map(ProviderId::Codex, Some(ReasoningEffort::Max)),
            json!({ "reasoning": { "effort": "xhigh", "summary": "auto" } })
        );
    }

    #[test]
    fn map_grok_effort() {
        assert_eq!(map(ProviderId::Grok, Some(ReasoningEffort::Medium)), json!({ "reasoning_effort": "medium" }));
        assert_eq!(map(ProviderId::Grok, Some(ReasoningEffort::None)), json!({ "reasoning_effort": "none" }));
        assert_eq!(map(ProviderId::Grok, Some(ReasoningEffort::XHigh)), json!({ "reasoning_effort": "xhigh" }));
    }

    #[test]
    fn map_clamps_grok_max_to_xhigh() {
        assert_eq!(map(ProviderId::Grok, Some(ReasoningEffort::Max)), json!({ "reasoning_effort": "xhigh" }));
    }

    #[test]
    fn map_antigravity_effort_to_thinking_level() {
        assert_eq!(
            map(ProviderId::Antigravity, Some(ReasoningEffort::Low)),
            json!({ "thinkingConfig": { "thinkingLevel": "low" } })
        );
        assert_eq!(
            map(ProviderId::Antigravity, Some(ReasoningEffort::High)),
            json!({ "thinkingConfig": { "thinkingLevel": "high" } })
        );
    }

    #[test]
    fn map_antigravity_none_to_zero_budget() {
        assert_eq!(
            map(ProviderId::Antigravity, Some(ReasoningEffort::None)),
            json!({ "thinkingConfig": { "thinkingBudget": 0 } })
        );
    }

    #[test]
    fn map_clamps_antigravity_above_ceiling_to_high() {
        for effort in [ReasoningEffort::XHigh, ReasoningEffort::Max] {
            assert_eq!(
                map(ProviderId::Antigravity, Some(effort)),
                json!({ "thinkingConfig": { "thinkingLevel": "high" } })
            );
        }
    }

    #[test]
    fn map_leaves_efforts_at_or_below_ceiling_unclamped() {
        assert_eq!(map(ProviderId::Grok, Some(ReasoningEffort::High)), json!({ "reasoning_effort": "high" }));
        assert_eq!(
            map(ProviderId::Codex, Some(ReasoningEffort::XHigh)),
            json!({ "reasoning": { "effort": "xhigh", "summary": "auto" } })
        );
        assert_eq!(
            map(ProviderId::ClaudeCode, Some(ReasoningEffort::Max)),
            json!({ "output_config": { "effort": "max" } })
        );
    }

    #[test]
    fn nearest_prefers_the_higher_token_on_equal_distance() {
        use ReasoningEffort::*;
        assert_eq!(nearest_reasoning_effort(High, &[Low, Medium, XHigh]), Some(XHigh));
    }

    #[test]
    fn nearest_maps_to_the_only_allowed_token() {
        use ReasoningEffort::*;
        assert_eq!(nearest_reasoning_effort(High, &[Medium]), Some(Medium));
        assert_eq!(nearest_reasoning_effort(Max, &[XHigh]), Some(XHigh));
        assert_eq!(nearest_reasoning_effort(ReasoningEffort::None, &[Low, Medium]), Some(Low));
    }

    #[test]
    fn nearest_is_identity_when_already_allowed() {
        use ReasoningEffort::*;
        assert_eq!(nearest_reasoning_effort(Medium, &[Medium, High]), Some(Medium));
    }

    #[test]
    fn nearest_is_none_for_an_empty_allowed_set() {
        assert_eq!(nearest_reasoning_effort(ReasoningEffort::High, &[]), Option::None);
    }

    #[test]
    fn serde_uses_the_wire_tokens() {
        assert_eq!(serde_json::to_value(ReasoningEffort::XHigh).unwrap(), json!("xhigh"));
        let parsed: ReasoningEffort = serde_json::from_value(json!("max")).unwrap();
        assert_eq!(parsed, ReasoningEffort::Max);
    }
}
