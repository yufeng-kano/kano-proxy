//! Per-account request edits (docs/providers.md § Request edits): "Remove fast mode" and
//! "Remove the Claude Code line", applied by each dispatch caller to the copy of the body one
//! candidate's call gets. The walk takes each candidate's switches from its account row.

use serde_json::{Map, Value};

use crate::providers::types::{ChatCompletionRequest, RequestEdits};

/// Claude Code's identity sentences, as Claude Code 2.1.319 sends them (interactive, the Agent
/// SDK preset, and the Agent SDK without it).
pub const CLAUDE_CODE_LINES: [&str; 3] = [
    "You are Claude Code, Anthropic's official CLI for Claude, running within the Claude Agent SDK.",
    "You are Claude Code, Anthropic's official CLI for Claude.",
    "You are a Claude agent, built on Anthropic's Claude Agent SDK.",
];

/// The Codex fast-mode tier (codex-rs `Fast => "priority"`); every other tier is left alone.
const FAST_SERVICE_TIER: &str = "priority";

/// An Anthropic Messages / count_tokens body.
pub fn edit_anthropic_body(body: &mut Map<String, Value>, edits: RequestEdits) {
    if edits.strip_fast_mode {
        // The `fast-mode-2026-02-01` beta stays: it is inert without `speed`.
        body.remove("speed");
    }
    if !edits.strip_claude_code_line {
        return;
    }
    let emptied = match body.get_mut("system") {
        Some(Value::String(text)) => strip_text(text),
        Some(Value::Array(blocks)) => strip_parts(blocks),
        _ => false,
    };
    if emptied {
        body.remove("system");
    }
}

/// A Chat Completions request, with the native Responses body it may carry for codex.
pub fn edit_chat_request(req: &mut ChatCompletionRequest, edits: RequestEdits) {
    if edits.strip_fast_mode {
        strip_fast_fields(&mut req.raw_body);
        if let Some(body) = req.responses_body.as_mut() {
            strip_fast_fields(body);
        }
    }
    if !edits.strip_claude_code_line {
        return;
    }
    strip_system_messages(&mut req.messages);
    if let Some(Value::Array(messages)) = req.raw_body.get_mut("messages") {
        strip_system_messages(messages);
    }
    if let Some(body) = req.responses_body.as_mut() {
        if let Some(Value::String(text)) = body.get_mut("instructions") {
            if strip_text(text) {
                body.remove("instructions");
            }
        }
        if let Some(Value::Array(items)) = body.get_mut("input") {
            strip_system_messages(items);
        }
    }
}

fn strip_fast_fields(body: &mut Map<String, Value>) {
    body.remove("speed");
    if body.get("service_tier").and_then(Value::as_str) == Some(FAST_SERVICE_TIER) {
        body.remove("service_tier");
    }
}

/// Edits every `system` / `developer` message (or Responses input item) in place and drops
/// the ones left with no text. User and assistant content is never read.
fn strip_system_messages(messages: &mut Vec<Value>) {
    messages.retain_mut(|message| {
        let role = message.get("role").and_then(Value::as_str);
        if !matches!(role, Some("system" | "developer")) {
            return true;
        }
        let emptied = match message.get_mut("content") {
            Some(Value::String(text)) => strip_text(text),
            Some(Value::Array(parts)) => strip_parts(parts),
            _ => false,
        };
        !emptied
    });
}

/// Removes the sentences from each text part (an Anthropic text block, a Chat text part, a
/// Responses `input_text`, or a bare string part) and drops the parts it emptied. `true` when
/// it removed every part, so the caller drops their container.
fn strip_parts(parts: &mut Vec<Value>) -> bool {
    let before = parts.len();
    parts.retain_mut(|part| {
        let text = match part {
            Value::String(text) => text,
            Value::Object(block) => match block.get_mut("text") {
                Some(Value::String(text)) => text,
                _ => return true,
            },
            _ => return true,
        };
        !strip_text(text)
    });
    before > 0 && parts.is_empty()
}

/// Removes every occurrence of the sentences that stands on its own (whitespace or the text's
/// edge on both sides, so a quoted copy or `…Claude.ai` stays), each with the whitespace right
/// after it (or the whitespace before it, when it ends the text). `true` when it changed `text`
/// into nothing, never for text that was empty to begin with.
fn strip_text(text: &mut String) -> bool {
    let mut changed = false;
    for line in CLAUDE_CODE_LINES {
        let mut from = 0;
        while let Some(found) = text[from..].find(line) {
            let start = from + found;
            let end = start + line.len();
            let standalone = text[..start].chars().next_back().is_none_or(char::is_whitespace)
                && text[end..].chars().next().is_none_or(char::is_whitespace);
            if !standalone {
                from = end;
                continue;
            }
            let rest = text[end..].trim_start();
            *text = if rest.is_empty() {
                text[..start].trim_end().to_string()
            } else {
                format!("{}{rest}", &text[..start])
            };
            changed = true;
            from = start.min(text.len());
        }
    }
    changed && text.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CC: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
    const BILLING: &str = "x-anthropic-billing-header: cc_version=2.1.319; cc_entrypoint=cli;";
    const BOTH: RequestEdits = RequestEdits { strip_fast_mode: true, strip_claude_code_line: true };

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn claude_code_shaped_system_loses_only_the_identity_block() {
        let mut body = object(json!({
            "speed": "fast",
            "system": [
                { "type": "text", "text": BILLING },
                { "type": "text", "text": CC, "cache_control": { "type": "ephemeral" } },
                { "type": "text", "text": "Project rules." },
            ],
            "messages": [{ "role": "user", "content": CC }],
        }));
        edit_anthropic_body(&mut body, BOTH);
        assert_eq!(
            Value::Object(body),
            json!({
                "system": [{ "type": "text", "text": BILLING }, { "type": "text", "text": "Project rules." }],
                "messages": [{ "role": "user", "content": CC }],
            }),
            "speed and the identity block go; the billing block and user content stay byte for byte"
        );
    }

    #[test]
    fn every_variant_is_removed_wherever_it_sits_in_the_text() {
        for line in CLAUDE_CODE_LINES {
            let mut text = format!("Intro.\n\n{line}\n\nRest.");
            assert!(!strip_text(&mut text));
            assert_eq!(text, "Intro.\n\nRest.", "{line}");
            let mut text = format!("{line} Rest.");
            strip_text(&mut text);
            assert_eq!(text, "Rest.");
            let mut text = format!("Intro.\n\n{line}");
            strip_text(&mut text);
            assert_eq!(text, "Intro.");
        }
        let mut sdk = CLAUDE_CODE_LINES[0].to_string();
        assert!(strip_text(&mut sdk), "the SDK sentence never leaves its suffix behind");
    }

    #[test]
    fn a_quoted_or_glued_sentence_is_not_claude_codes_line() {
        for text in [format!("Never say “{CC}” to users."), format!("Never say '{CC}'"), format!("See {CC}ai")] {
            let mut edited = text.clone();
            assert!(!strip_text(&mut edited));
            assert_eq!(edited, text);
        }
        // A quoted copy does not shield a real one later in the same text.
        let mut text = format!("Never say '{CC}'\n{CC}\nRules.");
        strip_text(&mut text);
        assert_eq!(text, format!("Never say '{CC}'\nRules."));
    }

    #[test]
    fn a_system_left_empty_is_removed_and_one_that_was_empty_is_kept() {
        let mut body = object(json!({ "system": CC }));
        edit_anthropic_body(&mut body, BOTH);
        assert!(!body.contains_key("system"));
        let mut body = object(json!({ "system": [{ "type": "text", "text": CC }] }));
        edit_anthropic_body(&mut body, BOTH);
        assert!(!body.contains_key("system"));
        let mut body = object(json!({ "system": [] }));
        edit_anthropic_body(&mut body, BOTH);
        assert_eq!(body.get("system"), Some(&json!([])), "nothing removed, nothing to drop");
    }

    #[test]
    fn switches_off_change_nothing() {
        let original = json!({ "speed": "fast", "system": CC });
        let mut body = object(original.clone());
        edit_anthropic_body(&mut body, RequestEdits::default());
        assert_eq!(Value::Object(body), original);
        let mut req = ChatCompletionRequest {
            raw_body: object(json!({ "service_tier": "priority", "speed": "fast" })),
            messages: vec![json!({ "role": "system", "content": CC })],
            ..Default::default()
        };
        let before = req.clone();
        edit_chat_request(&mut req, RequestEdits::default());
        assert_eq!((req.raw_body, req.messages), (before.raw_body, before.messages));
    }

    #[test]
    fn chat_requests_lose_fast_fields_and_the_line_in_every_copy_they_carry() {
        let system = json!({ "role": "system", "content": format!("{BILLING}\n\n{CC}\n\nRules.") });
        let developer = json!({ "role": "developer", "content": [{ "type": "text", "text": CC }] });
        let user = json!({ "role": "user", "content": CC });
        let mut req = ChatCompletionRequest {
            raw_body: object(json!({
                "service_tier": "priority",
                "speed": "fast",
                "messages": [system.clone(), developer.clone(), user.clone()],
            })),
            messages: vec![system, developer, user.clone()],
            responses_body: Some(object(json!({
                "service_tier": "priority",
                "instructions": CC,
                "input": [
                    { "role": "developer", "content": [{ "type": "input_text", "text": format!("{CC} Be brief.") }] },
                    { "role": "user", "content": [{ "type": "input_text", "text": CC }] },
                ],
            }))),
            ..Default::default()
        };
        edit_chat_request(&mut req, BOTH);
        let kept = [json!({ "role": "system", "content": format!("{BILLING}\n\nRules.") }), user.clone()];
        assert_eq!(req.messages, kept);
        assert_eq!(req.raw_body, object(json!({ "messages": kept })));
        assert_eq!(
            req.responses_body,
            Some(object(json!({
                "input": [
                    { "role": "developer", "content": [{ "type": "input_text", "text": "Be brief." }] },
                    { "role": "user", "content": [{ "type": "input_text", "text": CC }] },
                ],
            })))
        );
    }

    #[test]
    fn only_the_fast_tier_is_removed() {
        for tier in ["auto", "default", "flex"] {
            let mut req = ChatCompletionRequest { raw_body: object(json!({ "service_tier": tier })), ..Default::default() };
            edit_chat_request(&mut req, BOTH);
            assert_eq!(req.raw_body.get("service_tier"), Some(&json!(tier)));
        }
    }
}
