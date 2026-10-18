//! Parameters for the Anthropic-defined client tools a conversion keeps — docs/api.md
//! § `POST /anthropic/v1/messages`, "Anthropic-defined tools".
//!
//! A request declares the bash and text editor tools by `type` and `name` alone: Claude
//! already knows their parameters. A converted model does not, so they are written out here
//! as the parameters Anthropic documents for each version, and the tool then converts like a
//! client-defined one. Only the versions listed: a later one may change the parameters.

use std::borrow::Cow;

use serde_json::{json, Map, Value};

const BASH_DESCRIPTION: &str = "Run a command in a bash shell and get back its output. The shell persists between calls: the working directory, environment variables and files a command creates are still there for the next one.";

const TEXT_EDITOR_DESCRIPTION: &str =
    "View, create and edit text files. `command` picks the action; each other parameter says which commands use it.";

/// `tool` with a `description` and an `input_schema` when it is one of the bash or text
/// editor versions below, so the converters keep it; any other tool as it came.
pub fn with_schema(tool: &Map<String, Value>) -> Cow<'_, Map<String, Value>> {
    if tool.contains_key("input_schema") {
        return Cow::Borrowed(tool);
    }
    let (description, input_schema) = match tool.get("type").and_then(Value::as_str) {
        Some("bash_20241022" | "bash_20250124") => (BASH_DESCRIPTION, bash_schema()),
        Some("text_editor_20250429" | "text_editor_20250728") => {
            (TEXT_EDITOR_DESCRIPTION, text_editor_20250429_schema())
        }
        Some("text_editor_20241022" | "text_editor_20250124") => {
            (TEXT_EDITOR_DESCRIPTION, text_editor_20241022_schema())
        }
        _ => return Cow::Borrowed(tool),
    };
    let mut described = tool.clone();
    described.entry("description").or_insert_with(|| json!(description));
    described.insert("input_schema".into(), input_schema);
    Cow::Owned(described)
}

/// `bash_20241022`, `bash_20250124`: a command, or a restart.
fn bash_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": { "type": "string", "description": "The command to run. Required unless restart is true." },
            "restart": { "type": "boolean", "description": "true to restart the shell instead of running a command; the new shell starts clean." }
        }
    })
}

/// `text_editor_20250429`, `text_editor_20250728` (`str_replace_based_edit_tool`).
fn text_editor_20250429_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "enum": ["view", "create", "str_replace", "insert"],
                "description": "view: show a file with line numbers, or list a directory. create: write a new file. str_replace: replace one exact occurrence of a string. insert: add text after a line."
            },
            "path": { "type": "string", "description": "The file or directory to act on." },
            "view_range": {
                "type": "array",
                "items": { "type": "integer" },
                "minItems": 2,
                "maxItems": 2,
                "description": "view, files only: [start, end] line numbers, 1-indexed; an end of -1 reads to the end of the file."
            },
            "file_text": { "type": "string", "description": "create: the content of the new file." },
            "old_str": { "type": "string", "description": "str_replace: the text to replace. It must match exactly one place in the file, whitespace and indentation included." },
            "new_str": { "type": "string", "description": "str_replace: the text to put in its place." },
            "insert_line": { "type": "integer", "description": "insert: the line to insert after; 0 for the beginning of the file." },
            "insert_text": { "type": "string", "description": "insert: the text to insert." }
        },
        "required": ["command", "path"]
    })
}

/// `text_editor_20241022`, `text_editor_20250124` (`str_replace_editor`): `undo_edit` as
/// well, and `insert` takes its text in `new_str`.
fn text_editor_20241022_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "enum": ["view", "create", "str_replace", "insert", "undo_edit"],
                "description": "view: show a file with line numbers, or list a directory. create: write a new file. str_replace: replace one exact occurrence of a string. insert: add text after a line. undo_edit: revert the last edit of the file."
            },
            "path": { "type": "string", "description": "The file or directory to act on." },
            "view_range": {
                "type": "array",
                "items": { "type": "integer" },
                "minItems": 2,
                "maxItems": 2,
                "description": "view, files only: [start, end] line numbers, 1-indexed; an end of -1 reads to the end of the file."
            },
            "file_text": { "type": "string", "description": "create: the content of the new file." },
            "old_str": { "type": "string", "description": "str_replace: the text to replace. It must match exactly one place in the file, whitespace and indentation included." },
            "new_str": { "type": "string", "description": "str_replace: the text to put in its place. insert: the text to insert." },
            "insert_line": { "type": "integer", "description": "insert: the line to insert after; 0 for the beginning of the file." }
        },
        "required": ["command", "path"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("a tool object")
    }

    #[test]
    fn writes_out_the_bash_parameters() {
        let bash = tool(json!({ "type": "bash_20250124", "name": "bash" }));
        let described = with_schema(&bash);
        assert_eq!(described["name"], "bash");
        assert_eq!(described["description"], BASH_DESCRIPTION);
        assert_eq!(described["input_schema"]["properties"]["command"]["type"], "string");
        assert_eq!(described["input_schema"]["properties"]["restart"]["type"], "boolean");
    }

    #[test]
    fn writes_out_the_current_text_editor_parameters() {
        let editor = tool(json!({
            "type": "text_editor_20250728", "name": "str_replace_based_edit_tool", "max_characters": 10000
        }));
        let described = with_schema(&editor);
        let schema = &described["input_schema"];
        assert_eq!(described["name"], "str_replace_based_edit_tool");
        assert_eq!(schema["properties"]["command"]["enum"], json!(["view", "create", "str_replace", "insert"]));
        assert_eq!(schema["properties"]["insert_text"]["type"], "string");
        assert_eq!(schema["required"], json!(["command", "path"]));
    }

    #[test]
    fn the_older_text_editor_can_undo_and_inserts_new_str() {
        let editor = tool(json!({ "type": "text_editor_20250124", "name": "str_replace_editor" }));
        let schema = with_schema(&editor)["input_schema"].clone();
        assert_eq!(
            schema["properties"]["command"]["enum"],
            json!(["view", "create", "str_replace", "insert", "undo_edit"])
        );
        assert!(schema["properties"].get("insert_text").is_none());
    }

    #[test]
    fn keeps_a_description_the_client_gave() {
        let bash = tool(json!({ "type": "bash_20250124", "name": "bash", "description": "Shell in the sandbox" }));
        assert_eq!(with_schema(&bash)["description"], "Shell in the sandbox");
    }

    #[test]
    fn leaves_every_other_tool_as_it_came() {
        for value in [
            json!({ "type": "web_search_20250305", "name": "web_search", "max_uses": 5 }),
            json!({ "type": "computer_20250124", "name": "computer", "display_width_px": 1024, "display_height_px": 768 }),
            json!({ "type": "memory_20250818", "name": "memory" }),
            json!({ "type": "bash_20990101", "name": "bash" }),
            json!({ "type": "bash_20250124", "name": "bash", "input_schema": { "type": "object" } }),
            json!({ "name": "lookup", "input_schema": { "type": "object" } }),
        ] {
            let tool = tool(value);
            assert!(matches!(with_schema(&tool), Cow::Borrowed(_)), "{tool:?}");
        }
    }
}
