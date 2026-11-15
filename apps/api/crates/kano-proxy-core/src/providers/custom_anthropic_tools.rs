//! Anthropic-defined tools on a BYO Anthropic endpoint (docs/providers.md § Custom
//! providers, "same-account retry without Anthropic-defined tools, not a first-send edit").
//!
//! An ordered registry of recognizers for "tool N has no `input_schema`" in a 400 body, and
//! the rewrite the retry sends. A new server's wording is a new recognizer, not a policy
//! change.

use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};

use crate::proxy::anthropic_client_tools;

/// A body text recognizer: the index of the tool the endpoint refused.
pub type MissingSchemaParser = fn(&str) -> Option<usize>;

/// The error location, in the spellings a pydantic validator is reported in: a dotted path
/// (`body.tools.53.input_schema`), a tuple (`('body', 'tools', 53, 'input_schema')`) or a
/// JSON array, escaped or not (`["body","tools",53,"input_schema"]`).
static PYDANTIC_LOCATION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\btools\W{1,8}(\d{1,6})\W{1,8}input_schema\b").expect("tool location regex"));

/// The error kind beside it: pydantic's `'type': 'missing'` / `Field required`.
static PYDANTIC_MISSING_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\bfield required\b|\bmissing\b").expect("missing field regex"));

/// A FastAPI / pydantic server validating every tool as a client tool:
/// `1 validation error: {'type': 'missing', 'loc': 'body.tools.53.input_schema', 'msg': 'Field required', …}`.
pub fn parse_pydantic_missing_input_schema(body_text: &str) -> Option<usize> {
    if !PYDANTIC_MISSING_RE.is_match(body_text) {
        return None;
    }
    PYDANTIC_LOCATION_RE.captures(body_text)?.get(1)?.as_str().parse().ok()
}

const PARSERS: &[MissingSchemaParser] = &[parse_pydantic_missing_input_schema];

pub fn parse_missing_input_schema_rejection(body_text: &str) -> Option<usize> {
    PARSERS.iter().find_map(|parse| parse(body_text))
}

/// The body to send once more after `body_text` refused `sent_body`, or `None` when the
/// refusal is not about an Anthropic-defined tool this request declared.
pub fn without_anthropic_defined_tools(
    sent_body: &Map<String, Value>,
    body_text: &str,
) -> Option<Map<String, Value>> {
    let tools = sent_body.get("tools")?.as_array()?;
    let refused = parse_missing_input_schema_rejection(body_text)?;
    // The endpoint's word alone is not enough: a client tool sent without its schema is the
    // client's own error and goes back as it came.
    if !is_anthropic_defined(tools.get(refused)?.as_object()?) {
        return None;
    }

    let mut kept = Vec::with_capacity(tools.len());
    let mut removed: HashSet<&str> = HashSet::new();
    for tool in tools {
        let Some(map) = tool.as_object().filter(|map| is_anthropic_defined(map)) else {
            kept.push(tool.clone());
            continue;
        };
        let described = anthropic_client_tools::with_schema(map);
        if described.contains_key("input_schema") {
            kept.push(Value::Object(as_client_tool(&described)));
        } else if let Some(name) = map.get("name").and_then(Value::as_str) {
            removed.insert(name);
        }
    }

    let mut out = sent_body.clone();
    let forces_removed = sent_body
        .get("tool_choice")
        .and_then(|choice| choice.get("name"))
        .and_then(Value::as_str)
        .is_some_and(|name| removed.contains(name));
    if kept.is_empty() {
        out.remove("tools");
        out.remove("tool_choice");
    } else {
        out.insert("tools".into(), Value::Array(kept));
        if forces_removed {
            out.remove("tool_choice");
        }
    }
    Some(out)
}

/// Declared by `type` and `name` alone: the parameters are built into Claude
/// (docs/api.md § "Anthropic-defined tools").
fn is_anthropic_defined(tool: &Map<String, Value>) -> bool {
    !tool.contains_key("input_schema")
        && tool.get("type").and_then(Value::as_str).is_some_and(|tool_type| tool_type != "custom")
}

/// A bash or text editor tool with its parameters written out, as the client tool an
/// endpoint that does not know the Anthropic `type` can validate: the versioned `type` and
/// its options (`max_characters`) are left behind.
fn as_client_tool(described: &Map<String, Value>) -> Map<String, Value> {
    ["name", "description", "input_schema", "cache_control"]
        .into_iter()
        .filter_map(|key| described.get(key).map(|value| (key.to_string(), value.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// What Claude Code showed for a `format=anthropic` CLI provider, 2026-11-15.
    const PYDANTIC_DOTTED: &str = "1 validation error:\n    {'type': 'missing', 'loc': 'body.tools.1.input_schema', 'msg': 'Field required', 'input': '<dict of 3 items>'}";

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().expect("object").clone()
    }

    fn body() -> Map<String, Value> {
        object(json!({
            "model": "local",
            "max_tokens": 64,
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [
                { "name": "lookup", "description": "looks up", "input_schema": { "type": "object" } },
                { "type": "web_search_20250305", "name": "web_search", "max_uses": 5 },
            ],
        }))
    }

    #[test]
    fn reads_the_tool_index_from_each_pydantic_spelling() {
        for (text, index) in [
            (PYDANTIC_DOTTED.to_string(), 1),
            ("[{'type': 'missing', 'loc': ('body', 'tools', 53, 'input_schema'), 'msg': 'Field required'}]".into(), 53),
            (
                json!({ "detail": [{ "type": "missing", "loc": ["body", "tools", 7, "input_schema"], "msg": "Field required" }] })
                    .to_string(),
                7,
            ),
            // The same detail carried as a string inside another JSON envelope.
            (
                json!({ "error": { "message": json!([{ "type": "missing", "loc": ["body", "tools", 2, "input_schema"] }]).to_string() } })
                    .to_string(),
                2,
            ),
        ] {
            assert_eq!(parse_missing_input_schema_rejection(&text), Some(index), "{text}");
        }
    }

    #[test]
    fn other_400s_are_not_recognized() {
        for text in [
            // A schema that is present but wrong, in Anthropic's own wording.
            "tools.0.custom.input_schema: JSON schema is invalid. It must match JSON Schema draft 2020-12",
            // The right location without the missing-field kind.
            "{'type': 'dict_type', 'loc': 'body.tools.1.input_schema', 'msg': 'Input should be a valid dictionary'}",
            // The right kind on another field.
            "{'type': 'missing', 'loc': 'body.tools.1.name', 'msg': 'Field required'}",
            "{\"error\":{\"message\":\"max_tokens: Field required\"}}",
        ] {
            assert_eq!(parse_missing_input_schema_rejection(text), None, "{text}");
        }
    }

    #[test]
    fn removes_the_refused_tool_and_leaves_the_rest_of_the_body_alone() {
        let sent = body();
        let retry = without_anthropic_defined_tools(&sent, PYDANTIC_DOTTED).expect("a retry body");
        assert_eq!(retry["tools"], json!([sent["tools"][0].clone()]));
        let mut expected = sent.clone();
        expected.insert("tools".into(), retry["tools"].clone());
        assert_eq!(retry, expected);
    }

    #[test]
    fn removes_every_anthropic_defined_tool_not_only_the_reported_one() {
        let mut sent = body();
        sent.insert(
            "tools".into(),
            json!([
                { "type": "code_execution_20250825", "name": "code_execution" },
                { "type": "web_search_20250305", "name": "web_search", "max_uses": 5 },
                { "type": "custom", "name": "lookup", "input_schema": { "type": "object" } },
            ]),
        );
        let retry = without_anthropic_defined_tools(&sent, PYDANTIC_DOTTED).expect("a retry body");
        assert_eq!(retry["tools"], json!([{ "type": "custom", "name": "lookup", "input_schema": { "type": "object" } }]));
    }

    #[test]
    fn a_bash_or_text_editor_tool_stays_as_a_client_tool() {
        let mut sent = body();
        sent.insert(
            "tools".into(),
            json!([
                { "name": "lookup", "input_schema": { "type": "object" } },
                {
                    "type": "text_editor_20250728", "name": "str_replace_based_edit_tool", "max_characters": 10000,
                    "cache_control": { "type": "ephemeral" }
                },
            ]),
        );
        let retry = without_anthropic_defined_tools(&sent, PYDANTIC_DOTTED).expect("a retry body");
        let editor = object(retry["tools"][1].clone());
        let mut keys: Vec<&str> = editor.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["cache_control", "description", "input_schema", "name"]);
        assert_eq!(editor["name"], "str_replace_based_edit_tool");
        assert_eq!(editor["input_schema"]["required"], json!(["command", "path"]));
        assert_eq!(editor["cache_control"], json!({ "type": "ephemeral" }));
    }

    #[test]
    fn a_tool_choice_forcing_a_removed_tool_goes_with_it() {
        let mut sent = body();
        sent.insert("tool_choice".into(), json!({ "type": "tool", "name": "web_search" }));
        let retry = without_anthropic_defined_tools(&sent, PYDANTIC_DOTTED).expect("a retry body");
        assert!(!retry.contains_key("tool_choice"));

        sent.insert("tool_choice".into(), json!({ "type": "tool", "name": "lookup" }));
        let retry = without_anthropic_defined_tools(&sent, PYDANTIC_DOTTED).expect("a retry body");
        assert_eq!(retry["tool_choice"], json!({ "type": "tool", "name": "lookup" }));
    }

    #[test]
    fn no_tools_left_means_no_tools_and_no_tool_choice() {
        let mut sent = body();
        sent.insert("tools".into(), json!([{ "type": "web_search_20250305", "name": "web_search", "max_uses": 5 }]));
        sent.insert("tool_choice".into(), json!({ "type": "auto" }));
        let text = PYDANTIC_DOTTED.replace("tools.1.", "tools.0.");
        let retry = without_anthropic_defined_tools(&sent, &text).expect("a retry body");
        assert!(!retry.contains_key("tools"));
        assert!(!retry.contains_key("tool_choice"));
    }

    #[test]
    fn a_refusal_that_is_not_about_an_anthropic_defined_tool_is_left_alone() {
        // The index points at a client tool that has its schema.
        assert_eq!(without_anthropic_defined_tools(&body(), &PYDANTIC_DOTTED.replace("tools.1.", "tools.0.")), None);
        // The index is past the end.
        assert_eq!(without_anthropic_defined_tools(&body(), &PYDANTIC_DOTTED.replace("tools.1.", "tools.9.")), None);
        // A client tool sent without its schema: the client's own error.
        for broken in [json!({ "name": "lookup" }), json!({ "type": "custom", "name": "lookup" })] {
            let mut sent = body();
            sent.insert("tools".into(), json!([{ "name": "ok", "input_schema": {} }, broken]));
            assert_eq!(without_anthropic_defined_tools(&sent, PYDANTIC_DOTTED), None);
        }
        // No tools at all.
        let mut sent = body();
        sent.remove("tools");
        assert_eq!(without_anthropic_defined_tools(&sent, PYDANTIC_DOTTED), None);
    }
}
