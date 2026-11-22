//! Degenerate tool-call loop detection — conversion ingress only (grok / codex / a custom openai-format provider: any provider whose adapter
//! has no native Anthropic `messages()`). Never wired into native claude-code or custom
//! anthropic-format passthrough — see docs/api.md § "Degenerate tool-call loop guard" for the
//! full rationale and wire-up.
//!
//! Pure, synchronous, no I/O: walks the client's own message history for a trailing run of
//! identical tool-call/tool-result rounds.

use serde_json::Value;

/// Trailing identical run length that trips the guard (7 in a row is fine, 8 is not).
pub const LOOP_THRESHOLD: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopDetection {
    pub tripped: bool,
    /// Tool/function name of the trailing repeated call; `None` if no unit was found at all.
    pub name: Option<String>,
    /// Length of the trailing identical run (may be 0, and may be below [`LOOP_THRESHOLD`]).
    pub count: usize,
}

/// Shared 400 message text for both surfaces — see docs/api.md.
pub fn loop_detected_message(detection: &LoopDetection) -> String {
    format!(
        "degenerate tool-call loop detected: {} repeated {} times with identical input; aborting so the client can recover",
        detection.name.as_deref().unwrap_or("null"),
        detection.count
    )
}

fn is_role(msg: &Value, role: &str) -> bool {
    msg.get("role").and_then(Value::as_str) == Some(role)
}

struct AnthropicToolUse<'a> {
    id: &'a str,
    name: &'a str,
    input: Option<&'a Value>,
}

/// The lone `tool_use` block in an Anthropic assistant message's content array. `None` unless
/// content is an array containing EXACTLY one tool_use block (accompanying text/other blocks
/// are fine and ignored) with a string id and name.
fn single_anthropic_tool_use(content: Option<&Value>) -> Option<AnthropicToolUse<'_>> {
    let arr = content?.as_array()?;
    let mut tool_uses = arr
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"));
    let block = tool_uses.next()?;
    if tool_uses.next().is_some() {
        return None;
    }
    let id = block.get("id").and_then(Value::as_str)?;
    let name = block.get("name").and_then(Value::as_str)?;
    Some(AnthropicToolUse { id, name, input: block.get("input") })
}

/// Whether a user message's content array carries a tool_result for the given tool_use id.
fn has_anthropic_tool_result_for(content: Option<&Value>, tool_use_id: &str) -> bool {
    let Some(arr) = content.and_then(Value::as_array) else {
        return false;
    };
    arr.iter().any(|b| {
        b.get("type").and_then(Value::as_str) == Some("tool_result")
            && b.get("tool_use_id").and_then(Value::as_str) == Some(tool_use_id)
    })
}

/// Absent and `null` inputs both serialize as `{}`.
fn input_identity(input: Option<&Value>) -> String {
    match input {
        None | Some(Value::Null) => "{}".to_string(),
        Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
    }
}

/// Length of the trailing run of identical loop units, walking back two messages at a time.
/// `unit` reads one `(assistant, result)` pair as `(tool name, identity)`, or `None` when the
/// pair is not a complete loop unit — which ends the run.
fn trailing_run<'a>(messages: &'a [Value], unit: impl Fn(&'a Value, &'a Value) -> Option<(&'a str, String)>) -> LoopDetection {
    let mut count = 0usize;
    let mut reference: Option<(&str, String)> = None;
    for pair in messages.rchunks_exact(2) {
        let Some((name, identity)) = unit(&pair[0], &pair[1]) else { break };
        match &reference {
            None => reference = Some((name, identity)),
            Some((_, first)) if *first == identity => {}
            Some(_) => break,
        }
        count += 1;
    }
    LoopDetection { tripped: count >= LOOP_THRESHOLD, name: reference.map(|(name, _)| name.to_string()), count }
}

/// Anthropic Messages shape (`body.messages`, before conversion). A "loop unit" is an
/// assistant message whose content array contains exactly one tool_use block, immediately
/// followed by a user message carrying a tool_result for that block's id. tool_result
/// *contents* are never compared — only that a matching tool_result is present. Identity =
/// tool name + serialized input.
pub fn detect_anthropic_tool_loop(messages: &[Value]) -> LoopDetection {
    trailing_run(messages, |assistant, user| {
        if !is_role(user, "user") || !is_role(assistant, "assistant") {
            return None;
        }
        let tool_use = single_anthropic_tool_use(assistant.get("content"))?;
        if !has_anthropic_tool_result_for(user.get("content"), tool_use.id) {
            return None;
        }
        Some((tool_use.name, format!("{} {}", tool_use.name, input_identity(tool_use.input))))
    })
}

struct OpenAiToolCall<'a> {
    id: &'a str,
    name: &'a str,
    args: &'a str,
}

/// The lone entry in an OpenAI assistant message's `tool_calls` array.
fn single_openai_tool_call(tool_calls: Option<&Value>) -> Option<OpenAiToolCall<'_>> {
    let arr = tool_calls?.as_array()?;
    if arr.len() != 1 {
        return None;
    }
    let tc = arr.first()?;
    let function = tc.get("function");
    let id = tc.get("id").and_then(Value::as_str)?;
    let name = function.and_then(|f| f.get("name")).and_then(Value::as_str)?;
    let args = function.and_then(|f| f.get("arguments")).and_then(Value::as_str)?;
    Some(OpenAiToolCall { id, name, args })
}

/// OpenAI Chat Completions shape (`body.messages`). A "loop unit" is an assistant message
/// with exactly one `tool_calls` entry, immediately followed by a `role: "tool"` result for
/// that call's id. Identity = `function.name` + the raw `function.arguments` string (already
/// JSON text on the wire — never re-parsed or re-serialized).
pub fn detect_openai_tool_loop(messages: &[Value]) -> LoopDetection {
    trailing_run(messages, |assistant, tool| {
        if !is_role(tool, "tool") || !is_role(assistant, "assistant") {
            return None;
        }
        let call = single_openai_tool_call(assistant.get("tool_calls"))?;
        if tool.get("tool_call_id").and_then(Value::as_str) != Some(call.id) {
            return None;
        }
        Some((call.name, format!("{} {}", call.name, call.args)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn anthropic_unit(id: &str, name: &str, input: Value) -> Vec<Value> {
        vec![
            json!({ "role": "assistant", "content": [{ "type": "tool_use", "id": id, "name": name, "input": input }] }),
            json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": id, "content": format!("result-{id}") }] }),
        ]
    }

    fn anthropic_run(n: usize, name: &str, input: Value) -> Vec<Value> {
        let mut out = Vec::new();
        for i in 0..n {
            out.extend(anthropic_unit(&format!("toolu_{i}"), name, input.clone()));
        }
        out
    }

    fn default_run(n: usize) -> Vec<Value> {
        anthropic_run(n, "Read", json!({ "file_path": "/a" }))
    }

    fn openai_unit(id: &str, name: &str, args: &str) -> Vec<Value> {
        vec![
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": id, "type": "function", "function": { "name": name, "arguments": args } }],
            }),
            json!({ "role": "tool", "tool_call_id": id, "content": format!("result-{id}") }),
        ]
    }

    fn openai_run(n: usize, name: &str, args: &str) -> Vec<Value> {
        let mut out = Vec::new();
        for i in 0..n {
            out.extend(openai_unit(&format!("call_{i}"), name, args));
        }
        out
    }

    fn default_openai_run(n: usize) -> Vec<Value> {
        openai_run(n, "Read", r#"{"file_path":"/a"}"#)
    }

    #[test]
    fn anthropic_seven_identical_calls_does_not_trip() {
        let result = detect_anthropic_tool_loop(&default_run(7));
        assert!(!result.tripped);
        assert_eq!(result.count, 7);
    }

    /// `anthropic_unit` gives each tool_result distinct content: results never enter identity.
    #[test]
    fn anthropic_eight_identical_calls_trips() {
        let result = detect_anthropic_tool_loop(&default_run(8));
        assert!(result.tripped);
        assert_eq!(result.count, 8);
        assert_eq!(result.name.as_deref(), Some("Read"));
    }

    #[test]
    fn anthropic_differing_input_breaks_the_trailing_run() {
        let mut messages = anthropic_run(5, "Read", json!({ "file_path": "/other" }));
        messages.extend(anthropic_run(3, "Read", json!({ "file_path": "/a" })));
        let result = detect_anthropic_tool_loop(&messages);
        assert_eq!(result.count, 3);
        assert!(!result.tripped);
    }

    #[test]
    fn anthropic_differing_tool_name_breaks_the_trailing_run() {
        let mut messages = anthropic_run(5, "Write", json!({ "file_path": "/a" }));
        messages.extend(anthropic_run(3, "Read", json!({ "file_path": "/a" })));
        assert_eq!(detect_anthropic_tool_loop(&messages).count, 3);
    }

    #[test]
    fn anthropic_two_tool_use_blocks_break_the_run() {
        let mut messages = vec![
            json!({
                "role": "assistant",
                "content": [
                    { "type": "tool_use", "id": "a", "name": "Read", "input": { "file_path": "/a" } },
                    { "type": "tool_use", "id": "b", "name": "Read", "input": { "file_path": "/a" } },
                ],
            }),
            json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "a", "content": "ok" }] }),
        ];
        messages.extend(default_run(7));
        let result = detect_anthropic_tool_loop(&messages);
        assert_eq!(result.count, 7);
        assert!(!result.tripped);
    }

    #[test]
    fn anthropic_trailing_plain_text_turn_breaks_the_run_entirely() {
        let mut messages = default_run(8);
        messages.push(json!({ "role": "user", "content": "thanks!" }));
        let result = detect_anthropic_tool_loop(&messages);
        assert!(!result.tripped);
        assert_eq!(result.count, 0);
    }

    #[test]
    fn anthropic_accompanying_text_blocks_do_not_affect_identity() {
        let mut messages = Vec::new();
        for i in 0..8 {
            messages.push(json!({
                "role": "assistant",
                "content": [
                    { "type": "text", "text": format!("thinking {i}") },
                    { "type": "tool_use", "id": format!("t{i}"), "name": "Read", "input": { "file_path": "/a" } },
                ],
            }));
            messages.push(json!({
                "role": "user",
                "content": [{ "type": "tool_result", "tool_use_id": format!("t{i}"), "content": "ok" }],
            }));
        }
        assert!(detect_anthropic_tool_loop(&messages).tripped);
    }

    #[test]
    fn anthropic_mismatched_tool_result_id_does_not_complete_the_unit() {
        let mut messages = default_run(7);
        messages.push(json!({
            "role": "assistant",
            "content": [{ "type": "tool_use", "id": "mismatched", "name": "Read", "input": { "file_path": "/a" } }],
        }));
        messages.push(json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "someone-else", "content": "ok" }] }));
        assert_eq!(detect_anthropic_tool_loop(&messages).count, 0);
    }

    #[test]
    fn anthropic_empty_history_detects_nothing() {
        assert_eq!(
            detect_anthropic_tool_loop(&[]),
            LoopDetection { tripped: false, name: None, count: 0 }
        );
    }

    #[test]
    fn openai_seven_identical_calls_does_not_trip() {
        let result = detect_openai_tool_loop(&default_openai_run(7));
        assert!(!result.tripped);
        assert_eq!(result.count, 7);
    }

    /// `openai_unit` gives each tool result distinct content: results never enter identity.
    #[test]
    fn openai_eight_identical_calls_trips() {
        let result = detect_openai_tool_loop(&default_openai_run(8));
        assert!(result.tripped);
        assert_eq!(result.count, 8);
        assert_eq!(result.name.as_deref(), Some("Read"));
    }

    #[test]
    fn openai_differing_arguments_break_the_trailing_run() {
        let mut messages = openai_run(5, "Read", r#"{"file_path":"/other"}"#);
        messages.extend(openai_run(3, "Read", r#"{"file_path":"/a"}"#));
        let result = detect_openai_tool_loop(&messages);
        assert_eq!(result.count, 3);
        assert!(!result.tripped);
    }

    #[test]
    fn openai_two_tool_calls_entries_break_the_run() {
        let mut messages = vec![
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    { "id": "a", "type": "function", "function": { "name": "Read", "arguments": "{}" } },
                    { "id": "b", "type": "function", "function": { "name": "Read", "arguments": "{}" } },
                ],
            }),
            json!({ "role": "tool", "tool_call_id": "a", "content": "ok" }),
        ];
        messages.extend(default_openai_run(7));
        let result = detect_openai_tool_loop(&messages);
        assert_eq!(result.count, 7);
        assert!(!result.tripped);
    }

    #[test]
    fn openai_trailing_plain_text_turn_breaks_the_run_entirely() {
        let mut messages = default_openai_run(8);
        messages.push(json!({ "role": "user", "content": "thanks!" }));
        let result = detect_openai_tool_loop(&messages);
        assert!(!result.tripped);
        assert_eq!(result.count, 0);
    }

    #[test]
    fn formats_the_shared_400_message_text() {
        assert_eq!(
            loop_detected_message(&LoopDetection { tripped: true, name: Some("Read".into()), count: 8 }),
            "degenerate tool-call loop detected: Read repeated 8 times with identical input; aborting so the client can recover"
        );
    }
}
