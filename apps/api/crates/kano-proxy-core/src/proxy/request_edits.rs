//! Per-account request edits (docs/providers.md § Request edits): "Remove fast mode", "Hide
//! Claude Code identity" and "Keep prompt cache for 1 hour", applied by each dispatch caller to
//! the copy of the body one candidate's call gets. The walk takes each candidate's switches from
//! its account row.

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

/// Claude Code's attribution header, the text block Claude Code 2.1.319 puts first in `system`
/// and Anthropic reads to bill Claude Code usage. Only a leading block is one; a copy further
/// down is ordinary text.
pub const BILLING_HEADER_PREFIX: &str = "x-anthropic-billing-header:";

/// An Anthropic Messages / count_tokens body.
pub fn edit_anthropic_body(body: &mut Map<String, Value>, edits: RequestEdits) {
    if edits.force_cache_ttl_1h {
        force_one_hour_cache(body);
    }
    if edits.strip_fast_mode {
        // The `fast-mode-2026-02-01` beta stays: it is inert without `speed`.
        body.remove("speed");
    }
    if !edits.strip_claude_code_line {
        return;
    }
    let emptied = drop_leading_billing_header(body)
        || match body.get_mut("system") {
            Some(Value::String(text)) => strip_text(text),
            Some(Value::Array(blocks)) => strip_parts(blocks),
            _ => false,
        };
    if emptied {
        body.remove("system");
    }
}

/// Sets `ttl: "1h"` on every `cache_control` object the client placed: the top-level automatic
/// field, and any marker inside `tools`, `system` or `messages` however deeply it is nested
/// (a `tool_result`'s content, a `document`'s content blocks, a search result's blocks). A
/// marker left at 5m among 1h ones would break Anthropic's longer-TTL-first rule, so every one
/// is visited. A tool's `input_schema` and a `tool_use`'s `input` are client data that may hold
/// a key of that name and are skipped. No marker is added where there was none.
fn force_one_hour_cache(body: &mut Map<String, Value>) {
    fn visit(value: &mut Value) {
        match value {
            Value::Array(items) => items.iter_mut().for_each(visit),
            Value::Object(map) => {
                for (key, child) in map.iter_mut() {
                    match (key.as_str(), child) {
                        ("cache_control", Value::Object(marker)) => {
                            marker.insert("ttl".into(), Value::String("1h".into()));
                        }
                        ("input_schema" | "input", _) => {}
                        (_, child) => visit(child),
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(Value::Object(marker)) = body.get_mut("cache_control") {
        marker.insert("ttl".into(), Value::String("1h".into()));
    }
    for key in ["tools", "system", "messages"] {
        if let Some(value) = body.get_mut(key) {
            visit(value);
        }
    }
}

/// Removes the leading `x-anthropic-billing-header:` text block from `system` (docs/providers.md
/// § Request edits). A string `system` never carries one. Only the first text block counts.
/// `true` when it removed one and left no blocks behind, so the caller drops `system` entirely.
fn drop_leading_billing_header(body: &mut Map<String, Value>) -> bool {
    let Some(Value::Array(blocks)) = body.get_mut("system") else { return false };
    let leading = blocks.first().is_some_and(|block| {
        block.get("type").and_then(Value::as_str) == Some("text")
            && block.get("text").and_then(Value::as_str).is_some_and(|t| t.starts_with(BILLING_HEADER_PREFIX))
    });
    if !leading {
        return false;
    }
    blocks.remove(0);
    blocks.is_empty()
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
    const BOTH: RequestEdits = RequestEdits { strip_fast_mode: true, strip_claude_code_line: true, force_cache_ttl_1h: false };

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
                "system": [{ "type": "text", "text": "Project rules." }],
                "messages": [{ "role": "user", "content": CC }],
            }),
            "speed, the billing block and the identity block go; user content stays byte for byte"
        );
    }

    #[test]
    fn only_a_leading_billing_header_block_is_dropped() {
        // One further down is just text.
        let mut body = object(json!({
            "system": [
                { "type": "text", "text": "Project rules." },
                { "type": "text", "text": BILLING },
            ],
        }));
        edit_anthropic_body(&mut body, RequestEdits { strip_claude_code_line: true, ..Default::default() });
        assert_eq!(body["system"], json!([{ "type": "text", "text": "Project rules." }, { "type": "text", "text": BILLING }]));

        // A system left with nothing is removed entirely.
        let mut body = object(json!({ "system": [{ "type": "text", "text": BILLING }] }));
        edit_anthropic_body(&mut body, RequestEdits { strip_claude_code_line: true, ..Default::default() });
        assert!(body.get("system").is_none(), "the billing block alone leaves no system");
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
        for original in [
            json!({ "speed": "fast", "system": CC }),
            json!({ "system": [{ "type": "text", "text": BILLING }, { "type": "text", "text": CC }] }),
        ] {
            let mut body = object(original.clone());
            edit_anthropic_body(&mut body, RequestEdits::default());
            assert_eq!(Value::Object(body), original);
        }
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

    const ONE_HOUR: RequestEdits = RequestEdits { strip_fast_mode: false, strip_claude_code_line: false, force_cache_ttl_1h: true };

    fn cached_body() -> Map<String, Value> {
        let five = json!({ "type": "ephemeral" });
        object(json!({
            "cache_control": five,
            "tools": [
                {
                    "name": "write",
                    "input_schema": { "type": "object", "properties": { "cache_control": { "type": "object" } } },
                    "cache_control": five,
                },
                { "name": "read", "input_schema": { "type": "object" } },
            ],
            "system": [
                { "type": "text", "text": "rules", "cache_control": { "type": "ephemeral", "ttl": "5m" } },
                { "type": "text", "text": "more" },
            ],
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "q1", "cache_control": five }] },
                {
                    "role": "assistant",
                    "content": [{ "type": "tool_use", "id": "t1", "name": "write", "input": { "cache_control": "kept" } }],
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "t1",
                        "content": [{ "type": "text", "text": "done", "cache_control": five }],
                    }],
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "document",
                        "source": { "type": "content", "content": [{ "type": "text", "text": "page", "cache_control": five }] },
                    }],
                },
                { "role": "user", "content": "plain" },
            ],
        }))
    }

    #[test]
    fn the_cache_switch_gives_every_client_marker_an_hour() {
        let mut body = cached_body();
        edit_anthropic_body(&mut body, ONE_HOUR);
        let hour = json!({ "type": "ephemeral", "ttl": "1h" });
        assert_eq!(body["cache_control"], hour, "the top-level automatic field");
        assert_eq!(body["tools"][0]["cache_control"], hour);
        assert_eq!(body["system"][0]["cache_control"], hour, "a 5m marker is raised too");
        assert_eq!(body["messages"][0]["content"][0]["cache_control"], hour);
        assert_eq!(body["messages"][2]["content"][0]["content"][0]["cache_control"], hour, "inside a tool_result");
        assert_eq!(body["messages"][3]["content"][0]["source"]["content"][0]["cache_control"], hour, "inside a document");
        // Count every marker: nothing was added where the client placed none.
        assert_eq!(Value::Object(body.clone()).to_string().matches("\"ttl\":\"1h\"").count(), 6);
        assert!(body["tools"][1].get("cache_control").is_none() && body["system"][1].get("cache_control").is_none());
        assert_eq!(body["messages"][4]["content"], "plain");
    }

    #[test]
    fn the_cache_switch_leaves_client_data_and_everything_else_alone() {
        let mut body = cached_body();
        edit_anthropic_body(&mut body, ONE_HOUR);
        let before = cached_body();
        assert_eq!(body["tools"][0]["input_schema"], before["tools"][0]["input_schema"], "a schema property is not a marker");
        assert_eq!(body["messages"][1], before["messages"][1], "tool_use input is the model's data");

        let mut untouched = cached_body();
        edit_anthropic_body(&mut untouched, RequestEdits::default());
        assert_eq!(untouched, cached_body(), "off by default");
    }
}
