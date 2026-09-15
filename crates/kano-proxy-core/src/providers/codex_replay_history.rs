//! Port of apps/api/src/providers/codex_replay_history.ts (docs/providers.md § Codex).
//!
//! Match opaque reasoning to the exact visible assistant turn that produced it: each
//! completed turn is fingerprinted by the hash of the canonicalized visible Responses input
//! prefix before and after it, so an edited history, a changed tools/instructions/reasoning
//! configuration or a different account can never receive an unrelated record.

use serde_json::{Map, Value};

use super::codex_reasoning_cache::{
    hash_assistant_text, CodexReasoningReplayEntry, CodexReasoningReplayItem, CodexReasoningReplayTurn,
};

/// `JSON.stringify`-with-sorted-keys: the exact string the TypeScript `canonical` builds, so
/// the prefix hashes are stable across both editions.
fn canonical(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical(&map[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn item_type(item: &Value) -> Option<&str> {
    item.get("type").and_then(Value::as_str)
}

/// Response-only ids/status must not distinguish an echoed tool call, and an argument string
/// is compared by its parsed value so a re-serialized JSON object still matches.
fn visible_item(item: &Value) -> Value {
    if item_type(item) != Some("function_call") {
        return item.clone();
    }
    let mut out = Map::new();
    out.insert("type".into(), Value::String("function_call".into()));
    for key in ["call_id", "name"] {
        if let Some(v) = item.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    if let Some(args) = item.get("arguments") {
        let parsed = match args.as_str() {
            // Compare invalid JSON literally.
            Some(s) => serde_json::from_str::<Value>(s).unwrap_or_else(|_| args.clone()),
            None => args.clone(),
        };
        out.insert("arguments".into(), parsed);
    }
    Value::Object(out)
}

fn extend_hash(previous: &str, item: &Value) -> String {
    hash_assistant_text(&format!("{previous}\n{}", canonical(&visible_item(item))))
}

/// Hashes of the visible input prefix: `[0]` covers the stable request configuration, and
/// `[i + 1]` extends it with `input[i]`. Session secrets are never part of the material.
pub fn codex_input_hashes(body: &Map<String, Value>) -> Vec<String> {
    let mut config = Map::new();
    for key in ["instructions", "tools", "tool_choice", "reasoning", "text"] {
        if let Some(v) = body.get(key) {
            config.insert(key.into(), v.clone());
        }
    }
    let mut hashes = vec![hash_assistant_text(&canonical(&Value::Object(config)))];
    if let Some(Value::Array(input)) = body.get("input") {
        for item in input {
            let next = extend_hash(hashes.last().expect("seeded above"), item);
            hashes.push(next);
        }
    }
    hashes
}

/// The replay record for one completed assistant turn, or `None` when the turn carries no
/// reasoning, carries a custom tool call (which cannot round-trip through the Chat
/// conversion), or has no visible items to anchor to.
pub fn codex_replay_turn(
    start_hash: &str,
    items: &[CodexReasoningReplayItem],
    assistant_text: &str,
    shorten_call_id: &dyn Fn(&Value) -> Value,
) -> Option<CodexReasoningReplayTurn> {
    if !items.iter().any(|item| item_type(item) == Some("reasoning")) {
        return None;
    }
    // Custom tools cannot round-trip through our Chat conversion. Do not guess.
    if items.iter().any(|item| item_type(item) == Some("custom_tool_call")) {
        return None;
    }
    let mut calls: Vec<Value> = Vec::new();
    for item in items.iter().filter(|item| item_type(item) == Some("function_call")) {
        let mut call = Map::new();
        call.insert("type".into(), Value::String("function_call".into()));
        if let Some(id) = item.get("call_id") {
            call.insert("call_id".into(), shorten_call_id(id));
        }
        if let Some(name) = item.get("name") {
            call.insert("name".into(), name.clone());
        }
        let args = item.get("arguments").filter(|v| !v.is_null()).cloned();
        call.insert("arguments".into(), args.unwrap_or_else(|| Value::String("{}".into())));
        calls.push(Value::Object(call));
    }
    let mut visible: Vec<Value> = Vec::new();
    if !assistant_text.is_empty() {
        visible.push(serde_json::json!({
            "role": "assistant",
            "content": [{ "type": "output_text", "text": assistant_text }],
        }));
    }
    visible.extend(calls.iter().cloned());
    if visible.is_empty() {
        return None;
    }
    let mut end_hash = start_hash.to_string();
    for item in &visible {
        end_hash = extend_hash(&end_hash, item);
    }
    let mut stored: Vec<Value> =
        items.iter().filter(|item| item_type(item) == Some("reasoning")).cloned().collect();
    stored.extend(calls);
    Some(CodexReasoningReplayTurn {
        start_hash: start_hash.to_string(),
        end_hash,
        visible_count: visible.len(),
        items: stored,
    })
}

/// Reinsert every matched turn's reasoning at its exact assistant boundary, returning the
/// rewritten `input` and the history that is still anchored in it.
pub fn replay_codex_history(
    input: &[Value],
    hashes: &[String],
    history: Option<&CodexReasoningReplayEntry>,
) -> (Vec<Value>, CodexReasoningReplayEntry) {
    let position = |hash: &str| hashes.iter().position(|h| h == hash);
    let mut matches: Vec<(usize, &CodexReasoningReplayTurn)> = Vec::new();
    for turn in history.map(|h| h.turns.as_slice()).unwrap_or(&[]) {
        let Some(start) = position(&turn.start_hash) else { continue };
        if hashes.get(start + turn.visible_count).map(String::as_str) != Some(turn.end_hash.as_str()) {
            continue;
        }
        let end = (start + turn.visible_count).min(input.len());
        if input[start.min(input.len())..end].iter().any(|item| item_type(item) == Some("reasoning")) {
            continue;
        }
        matches.push((start, turn));
    }

    let mut output: Vec<Value> = Vec::new();
    let mut retained: Vec<CodexReasoningReplayTurn> = Vec::new();
    let mut index = 0usize;
    while index < input.len() {
        // Later records win at the same boundary, as the TypeScript `Map.set` does.
        let Some((_, turn)) = matches.iter().rev().find(|(start, _)| *start == index) else {
            output.push(input[index].clone());
            index += 1;
            continue;
        };
        retained.push((*turn).clone());
        output.extend(turn.items.iter().filter(|item| item_type(item) == Some("reasoning")).cloned());
        let calls: Vec<&Value> =
            turn.items.iter().filter(|item| item_type(item) == Some("function_call")).collect();
        // Restore original argument serialization, without duplicating tool calls.
        let end = (index + turn.visible_count).min(input.len());
        for item in &input[index..end] {
            if item_type(item) == Some("function_call") {
                let replacement = calls.iter().find(|call| call.get("call_id") == item.get("call_id"));
                output.push(replacement.map(|c| (*c).clone()).unwrap_or_else(|| item.clone()));
            } else {
                output.push(item.clone());
            }
        }
        index += turn.visible_count;
    }
    (output, CodexReasoningReplayEntry { turns: retained })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::codex_reasoning_cache::append_codex_replay_turn;
    use serde_json::json;

    fn identity(id: &Value) -> Value {
        id.clone()
    }

    fn reasoning() -> Value {
        json!({ "type": "reasoning", "encrypted_content": "opaque-test" })
    }

    fn body() -> Map<String, Value> {
        json!({ "instructions": "stable", "input": [{ "role": "user", "content": "task" }] })
            .as_object()
            .unwrap()
            .clone()
    }

    fn body_with(extra: Value, input: &[Value]) -> Map<String, Value> {
        let mut b = body();
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                b.insert(k.clone(), v.clone());
            }
        }
        b.insert("input".into(), Value::Array(input.to_vec()));
        b
    }

    fn input_of(body: &Map<String, Value>) -> Vec<Value> {
        body.get("input").and_then(Value::as_array).cloned().unwrap_or_default()
    }

    #[test]
    fn rejects_tool_instruction_and_effort_changes_even_when_visible_history_matches() {
        let hashes = codex_input_hashes(&body());
        let turn = codex_replay_turn(hashes.last().unwrap(), &[reasoning()], "answer", &identity).unwrap();
        let mut input = input_of(&body());
        input.push(json!({ "role": "assistant", "content": [{ "type": "output_text", "text": "answer" }] }));
        let history = CodexReasoningReplayEntry { turns: vec![turn] };
        for changed in [
            json!({ "instructions": "edited" }),
            json!({ "tools": [{ "type": "function", "name": "new" }] }),
            json!({ "reasoning": { "effort": "high" } }),
        ] {
            let changed_hashes = codex_input_hashes(&body_with(changed, &input));
            let (out, retained) = replay_codex_history(&input, &changed_hashes, Some(&history));
            assert_eq!(out, input);
            assert!(retained.turns.is_empty());
        }
    }

    #[test]
    fn matches_tool_only_calls_by_arguments_and_id_not_empty_assistant_text() {
        let hashes = codex_input_hashes(&body());
        let call = json!({ "type": "function_call", "call_id": "one", "name": "read", "arguments": "{\"a\":1}" });
        let turn =
            codex_replay_turn(hashes.last().unwrap(), &[reasoning(), call.clone()], "", &identity).unwrap();
        let history = CodexReasoningReplayEntry { turns: vec![turn] };
        for changed in [json!({ "call_id": "two" }), json!({ "arguments": "{\"a\":2}" }), json!({ "name": "write" })] {
            let mut edited = call.as_object().unwrap().clone();
            for (k, v) in changed.as_object().unwrap() {
                edited.insert(k.clone(), v.clone());
            }
            let mut input = input_of(&body());
            input.push(Value::Object(edited));
            let hashes = codex_input_hashes(&body_with(json!({}), &input));
            assert_eq!(replay_codex_history(&input, &hashes, Some(&history)).0, input);
        }
    }

    #[test]
    fn replays_a_matching_turn_at_its_boundary() {
        let hashes = codex_input_hashes(&body());
        let turn = codex_replay_turn(hashes.last().unwrap(), &[reasoning()], "answer", &identity).unwrap();
        let mut input = input_of(&body());
        input.push(json!({ "role": "assistant", "content": [{ "type": "output_text", "text": "answer" }] }));
        let hashes = codex_input_hashes(&body_with(json!({}), &input));
        let history = CodexReasoningReplayEntry { turns: vec![turn.clone()] };
        let (out, retained) = replay_codex_history(&input, &hashes, Some(&history));
        assert_eq!(out[0], input[0]);
        assert_eq!(out[1], reasoning());
        assert_eq!(out[2], input[1]);
        assert_eq!(retained.turns, vec![turn]);
    }

    #[test]
    fn keeps_an_older_prefix_rather_than_evicting_it_for_an_oversized_new_turn() {
        let hashes = codex_input_hashes(&body());
        let turn = codex_replay_turn(hashes.last().unwrap(), &[reasoning()], "answer", &identity).unwrap();
        let history = CodexReasoningReplayEntry { turns: vec![turn.clone()] };
        let large = CodexReasoningReplayTurn {
            start_hash: "new-start".into(),
            items: vec![json!({ "type": "reasoning", "encrypted_content": "x".repeat(300_000) })],
            ..turn.clone()
        };
        assert_eq!(append_codex_replay_turn(&history, Some(large)), history);
        assert_eq!(append_codex_replay_turn(&history, None), history);
        assert_eq!(append_codex_replay_turn(&history, Some(turn)), history);
    }

    #[test]
    fn a_turn_without_reasoning_or_with_a_custom_tool_call_is_not_recorded() {
        let hashes = codex_input_hashes(&body());
        let start = hashes.last().unwrap();
        assert!(codex_replay_turn(start, &[json!({ "type": "message" })], "answer", &identity).is_none());
        assert!(codex_replay_turn(
            start,
            &[reasoning(), json!({ "type": "custom_tool_call", "call_id": "c" })],
            "answer",
            &identity
        )
        .is_none());
        // Reasoning with neither assistant text nor calls has nothing visible to anchor to.
        assert!(codex_replay_turn(start, &[reasoning()], "", &identity).is_none());
    }

    #[test]
    fn canonical_sorts_keys_and_ignores_argument_serialization() {
        assert_eq!(canonical(&json!({ "b": 1, "a": [1, "x", null] })), r#"{"a":[1,"x",null],"b":1}"#);
        let a = json!({ "type": "function_call", "call_id": "1", "arguments": "{\"a\":1,\"b\":2}" });
        let b = json!({ "type": "function_call", "call_id": "1", "arguments": "{ \"b\": 2, \"a\": 1 }" });
        assert_eq!(canonical(&visible_item(&a)), canonical(&visible_item(&b)));
    }
}
