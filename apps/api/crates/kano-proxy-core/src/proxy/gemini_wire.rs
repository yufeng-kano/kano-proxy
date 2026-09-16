//! Gemini wire details (docs/providers.md § Antigravity).
//!
//! Shared Gemini `GenerateContent` wire shapes and the pieces both antigravity
//! conversion surfaces need (`gemini_openai.rs` for `/openai/v1`,
//! `gemini_anthropic.rs` for `/anthropic`).
//!
//! Field names follow the proto-JSON camelCase spelling the CloudCode backend
//! emits; see CLIProxyAPI `internal/translator/antigravity/**` for the
//! translator behaviour this mirrors. Nothing here knows about the antigravity
//! envelope (`{model, project, request, …}`) — that lives in the adapter.
//!
//! JSON is `serde_json::Value` throughout (`preserve_order` is on) so emitted
//! key order matches the JavaScript objects byte for byte.

use futures::stream::StreamExt;
use serde_json::{Map, Value};

use crate::upstream::transport::ByteStream;

const EMPTY_PARTS: &[Value] = &[];

/// Gemini validates `contents` from the top: a `functionCall` in a model turn
/// must follow a user turn, and the first content has nothing before it, so a
/// history opening with a model turn is rejected outright ("Please ensure that
/// function call turn comes immediately after a user turn or after a function
/// response turn"). Anthropic accepts an assistant-first conversation, and
/// agents that seed a cache-stable prefix with synthetic tool calls send one.
/// The text is non-empty because Gemini also rejects an empty text part.
pub fn open_with_user_turn(contents: &mut Vec<Value>) {
    let opens_with_model = contents
        .first()
        .and_then(|c| c.get("role"))
        .and_then(Value::as_str)
        == Some("model");
    if opens_with_model {
        contents.insert(
            0,
            serde_json::json!({ "role": "user", "parts": [{ "text": "(conversation start)" }] }),
        );
    }
}

/// Every antigravity response — stream frame or whole body — is `{response: …}`.
pub fn unwrap_antigravity_response(json: &Value) -> Option<&Value> {
    if !(json.is_object() || json.is_array()) {
        return None;
    }
    match json.get("response") {
        Some(inner) if inner.is_object() || inner.is_array() => Some(inner),
        // A bare Gemini response (no envelope) is still usable — countTokens and
        // the occasional error frame come through unwrapped.
        _ => Some(json),
    }
}

pub fn gemini_parts(resp: Option<&Value>) -> &[Value] {
    resp.and_then(|r| r.get("candidates"))
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(EMPTY_PARTS)
}

/// The first candidate's `finishReason`, when it has one.
pub fn first_finish_reason(resp: Option<&Value>) -> Option<&str> {
    resp.and_then(|r| r.get("candidates"))
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("finishReason"))
        .and_then(Value::as_str)
}

/// `promptFeedback.blockReason`, when the prompt itself was blocked.
pub fn block_reason(resp: Option<&Value>) -> Option<&str> {
    resp.and_then(|r| r.get("promptFeedback"))
        .and_then(|f| f.get("blockReason"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// A response with no candidates at all (`candidates` missing or empty).
pub fn has_no_candidates(resp: Option<&Value>) -> bool {
    resp.and_then(|r| r.get("candidates"))
        .and_then(Value::as_array)
        .map(|c| c.is_empty())
        .unwrap_or(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineData {
    pub mime_type: String,
    pub data: String,
}

/// Both spellings appear in the wild; the backend accepts and emits either.
pub fn inline_data_of(part: &Value) -> Option<InlineData> {
    let raw = part
        .get("inlineData")
        .or_else(|| part.get("inline_data"))
        .filter(|v| !v.is_null())?;
    let data = raw.get("data").and_then(Value::as_str).unwrap_or("");
    if data.is_empty() {
        return None;
    }
    let mime_type = raw
        .get("mimeType")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            raw.get("mime_type")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("image/png");
    Some(InlineData { mime_type: mime_type.to_string(), data: data.to_string() })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NormalizedGeminiUsage {
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub cached_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

/// The TypeScript `num()`: a finite JSON number, otherwise null. Token counts
/// are integers on this wire, so a fractional value is truncated rather than
/// re-emitted as a float (which would not match the JavaScript output).
fn num(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64)),
        _ => None,
    }
}

/// `candidatesTokenCount` counts only the visible answer; `thoughtsTokenCount`
/// is billed output too but is reported separately, so both surfaces add them
/// together for their own output figure and keep the thinking half visible in
/// the details field their format has for it.
pub fn normalize_gemini_usage(usage: Option<&Value>) -> Option<NormalizedGeminiUsage> {
    let usage = usage?;
    if !usage.is_object() {
        return None;
    }
    Some(NormalizedGeminiUsage {
        prompt_tokens: num(usage.get("promptTokenCount")),
        completion_tokens: num(usage.get("candidatesTokenCount")),
        reasoning_tokens: num(usage.get("thoughtsTokenCount")),
        cached_tokens: num(usage.get("cachedContentTokenCount")),
        total_tokens: num(usage.get("totalTokenCount")),
    })
}

/// Candidate-level terminal reasons that mean the *output* was blocked. These
/// must not read as a successful `stop` / `end_turn` — an often-empty answer a
/// client cannot tell apart from a real blank completion.
const BLOCKED_FINISH_REASONS: &[&str] = &[
    "SAFETY",
    "RECITATION",
    "BLOCKLIST",
    "PROHIBITED_CONTENT",
    "SPII",
    "IMAGE_SAFETY",
];

/// Gemini `finishReason` → the OpenAI token, before the tool-call override the
/// callers apply (a turn that produced a `functionCall` is `tool_calls`
/// whatever the upstream reason said).
pub fn openai_finish_reason(finish_reason: Option<&str>) -> &'static str {
    let reason = finish_reason.unwrap_or("").to_ascii_uppercase();
    if reason == "MAX_TOKENS" {
        return "length";
    }
    if BLOCKED_FINISH_REASONS.contains(&reason.as_str()) {
        return "content_filter";
    }
    "stop"
}

/// Gemini `finishReason` → the Anthropic `stop_reason` token.
pub fn anthropic_stop_reason(finish_reason: Option<&str>, saw_tool_call: bool) -> &'static str {
    if saw_tool_call {
        return "tool_use";
    }
    let reason = finish_reason.unwrap_or("").to_ascii_uppercase();
    if reason == "MAX_TOKENS" {
        return "max_tokens";
    }
    if BLOCKED_FINISH_REASONS.contains(&reason.as_str()) {
        return "refusal";
    }
    "end_turn"
}

// ── SSE line splitting ─────────────────────────────────────────────────────

/// Split an SSE body into `data:` payload strings without buffering the whole
/// stream — the shared `proxy::sse_lines` reader (16 MiB line budget) supplies
/// the lines. `[DONE]` is yielded as-is so callers can end cleanly.
pub struct SseDataLines {
    lines: crate::proxy::sse_lines::SseLines<ByteStream>,
}

impl SseDataLines {
    pub fn new(body: ByteStream) -> Self {
        Self { lines: crate::proxy::sse_lines::read_sse_lines_default(body) }
    }

    /// The next `data:` payload, `None` at end of stream.
    pub async fn next_data(&mut self) -> Option<Result<String, std::io::Error>> {
        loop {
            match self.lines.next().await? {
                Err(e) => return Some(Err(e)),
                Ok(line) => {
                    if let Some(rest) = line.strip_prefix("data:") {
                        let data = rest.trim();
                        if !data.is_empty() {
                            return Some(Ok(data.to_string()));
                        }
                    }
                }
            }
        }
    }
}

// ── JSON Schema sanitization ───────────────────────────────────────────────

/// The fields Gemini's `Schema` actually has, read from the Antigravity CLI's
/// embedded descriptor for `google.cloud.aiplatform.master.Schema` (extracted
/// 2026-08-22). This is an **allowlist**, and deliberately so: the backend
/// rejects the whole request with a `400` naming any field its proto lacks, so
/// a denylist fails open — every JSON Schema keyword nobody thought of is an
/// outage. `propertyNames` was exactly that (Claude Code sends it; every tool
/// call 400'd until it was added).
///
/// Fields the proto *does* have but that are annotated `GOOGLE_INTERNAL` — and
/// are rejected for external callers — are omitted here on purpose:
/// `prefix_items`, `one_of`, `all_of`, `additional_properties_schema`. So are
/// `additionalProperties`, `title`, `default` and `defs`, which the backend
/// rejects in practice (the first is the one that bites: OpenAI clients set it
/// on every strict tool).
const SUPPORTED_SCHEMA_KEYS: &[&str] = &[
    "type",
    "format",
    "description",
    "nullable",
    "enum",
    "items",
    "minItems",
    "maxItems",
    "properties",
    "propertyOrdering",
    "required",
    "minProperties",
    "maxProperties",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "pattern",
    "example",
    "anyOf",
];

/// `{"type": "null"}` and nothing else — JSON Schema's way of saying nullable.
fn is_null_schema(v: &Value) -> bool {
    match v {
        Value::Object(map) => map.len() == 1 && map.get("type") == Some(&Value::String("null".into())),
        _ => false,
    }
}

/// JSON Schema spells "nullable" two ways Gemini's `Schema` cannot hold, because
/// its `type` is an enum with no NULL member: `anyOf: [X, {"type": "null"}]` and
/// `type: ["string", "null"]`. Forwarding either reaches Claude-behind-Antigravity
/// as a schema its own validator rejects (`tools.N.custom.input_schema: JSON
/// schema is invalid`, measured 2026-08-22), so both are folded into the
/// `nullable: true` field Gemini does have.
fn fold_nullable(mut out: Map<String, Value>) -> Map<String, Value> {
    if let Some(Value::Array(types)) = out.get("type").cloned() {
        let kept: Vec<Value> = types.iter().filter(|t| t.as_str() != Some("null")).cloned().collect();
        if kept.len() < types.len() {
            out.insert("nullable".into(), Value::Bool(true));
        }
        // More than one non-null type has no Gemini representation either; keeping
        // the first is closer than emitting an array the backend will reject.
        match kept.first() {
            Some(first) => {
                out.insert("type".into(), first.clone());
            }
            None => {
                out.shift_remove("type");
            }
        }
    }
    if let Some(Value::Array(members)) = out.get("anyOf").cloned() {
        let kept: Vec<Value> = members.iter().filter(|m| !is_null_schema(m)).cloned().collect();
        if kept.len() < members.len() {
            out.insert("nullable".into(), Value::Bool(true));
        }
        if kept.len() == 1 && kept[0].is_object() {
            // A single remaining branch is the schema itself; anyOf around one member
            // is noise, and Gemini rejects an anyOf that also carries sibling fields.
            let mut rest = out;
            rest.shift_remove("anyOf");
            let mut merged = kept[0].as_object().cloned().unwrap_or_default();
            for (k, v) in rest {
                merged.insert(k, v);
            }
            return merged;
        }
        if !kept.is_empty() {
            out.insert("anyOf".into(), Value::Array(kept));
        } else {
            out.shift_remove("anyOf");
        }
    }
    out
}

/// Per-model-family schema limits. Measured against Antigravity 2026-08-22 with
/// a two-branch `anyOf` on a tool parameter: a **Gemini** model accepts it, a
/// **Claude** model behind the same endpoint answers `tools.N.custom
/// .input_schema: JSON schema is invalid` from its own Vertex-side validator.
/// Google's Gemini→Claude translation evidently cannot carry the union, so on
/// that family the union is dropped and the parameter is left unconstrained —
/// a typeless property is accepted by both, and the description survives to
/// guide the model. Same shape of rule as VALIDATED function calling
/// (docs/providers.md § Antigravity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaDialect {
    pub allow_any_of: bool,
}

pub const GEMINI_SCHEMA_DIALECT: SchemaDialect = SchemaDialect { allow_any_of: true };
pub const CLAUDE_SCHEMA_DIALECT: SchemaDialect = SchemaDialect { allow_any_of: false };

/// `claude` in the upstream model id — the same test the request builder uses.
pub fn schema_dialect_for(model: &str) -> SchemaDialect {
    if model.to_lowercase().contains("claude") {
        CLAUDE_SCHEMA_DIALECT
    } else {
        GEMINI_SCHEMA_DIALECT
    }
}

pub fn sanitize_json_schema(schema: &Value, dialect: SchemaDialect) -> Value {
    if let Value::Array(items) = schema {
        return Value::Array(items.iter().map(|v| sanitize_json_schema(v, dialect)).collect());
    }
    let Value::Object(obj) = schema else {
        return schema.clone();
    };
    let mut out = Map::new();
    for (key, value) in obj {
        if !SUPPORTED_SCHEMA_KEYS.contains(&key.as_str()) {
            continue;
        }
        // `properties` maps arbitrary *property names* to schemas — the names are
        // not schema keywords, so a property that happens to be called "title" or
        // "default" must survive while its schema value is still sanitized.
        if key == "properties" {
            if let Value::Object(props) = value {
                let mut sanitized = Map::new();
                for (name, sub) in props {
                    sanitized.insert(name.clone(), sanitize_json_schema(sub, dialect));
                }
                out.insert(key.clone(), Value::Object(sanitized));
                continue;
            }
        }
        out.insert(key.clone(), sanitize_json_schema(value, dialect));
    }
    let mut folded = fold_nullable(out);
    // A surviving multi-branch anyOf is a genuine union; fold_nullable already
    // collapsed the `[X, null]` spelling into a single schema.
    if !dialect.allow_any_of && folded.get("anyOf").is_some_and(Value::is_array) {
        folded.shift_remove("anyOf");
    }
    // Gemini requires `items` on every array (`400 …properties[x].items: missing
    // field`), and the allowlist can strip an array down to that shape: a JSON
    // Schema tuple spelled `prefixItems` (GOOGLE_INTERNAL, dropped above) or the
    // draft-04 `items: [...]` (an array where the proto wants one schema).
    // Measured 2026-09-04 with Claude Code's Artifact tool (`where` triples).
    // An unconstrained element schema is valid on both model families.
    let is_array_type = folded.get("type") == Some(&Value::String("array".into()));
    let items_unusable = folded.get("items").is_none_or(Value::is_array);
    if is_array_type && items_unusable {
        folded.insert("items".into(), Value::Object(Map::new()));
    }
    Value::Object(folded)
}

/// `crypto.randomUUID().replace(/-/g, "").slice(0, n)`.
pub(crate) fn random_hex(n: usize) -> String {
    let id = crate::ids::new_id("");
    id[..n.min(id.len())].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use serde_json::json;

    fn sanitize(v: Value) -> Value {
        sanitize_json_schema(&v, GEMINI_SCHEMA_DIALECT)
    }

    #[test]
    fn unwraps_the_envelope_and_a_bare_response() {
        let wrapped = json!({ "response": { "candidates": [] } });
        assert_eq!(unwrap_antigravity_response(&wrapped), Some(&wrapped["response"]));
        let bare = json!({ "candidates": [] });
        assert_eq!(unwrap_antigravity_response(&bare), Some(&bare));
        assert_eq!(unwrap_antigravity_response(&json!("x")), None);
    }

    #[test]
    fn reads_both_inline_data_spellings() {
        assert_eq!(
            inline_data_of(&json!({ "inline_data": { "mime_type": "image/jpeg", "data": "AA" } })),
            Some(InlineData { mime_type: "image/jpeg".into(), data: "AA".into() })
        );
        assert_eq!(
            inline_data_of(&json!({ "inlineData": { "data": "AA" } })),
            Some(InlineData { mime_type: "image/png".into(), data: "AA".into() })
        );
        assert_eq!(inline_data_of(&json!({ "inlineData": { "data": "" } })), None);
        assert_eq!(inline_data_of(&json!({ "text": "hi" })), None);
    }

    #[test]
    fn opens_an_assistant_first_history_with_a_user_turn() {
        let mut contents = vec![json!({ "role": "model", "parts": [] })];
        open_with_user_turn(&mut contents);
        assert_eq!(contents[0], json!({ "role": "user", "parts": [{ "text": "(conversation start)" }] }));
        let mut already = vec![json!({ "role": "user", "parts": [] })];
        open_with_user_turn(&mut already);
        assert_eq!(already.len(), 1);
    }

    #[test]
    fn maps_finish_reasons() {
        assert_eq!(openai_finish_reason(Some("MAX_TOKENS")), "length");
        assert_eq!(openai_finish_reason(Some("SAFETY")), "content_filter");
        assert_eq!(openai_finish_reason(Some("STOP")), "stop");
        assert_eq!(openai_finish_reason(None), "stop");
        assert_eq!(anthropic_stop_reason(Some("STOP"), true), "tool_use");
        assert_eq!(anthropic_stop_reason(Some("MAX_TOKENS"), false), "max_tokens");
        assert_eq!(anthropic_stop_reason(Some("RECITATION"), false), "refusal");
        assert_eq!(anthropic_stop_reason(None, false), "end_turn");
    }

    #[test]
    fn normalizes_usage() {
        let usage = normalize_gemini_usage(Some(&json!({
            "promptTokenCount": 10,
            "candidatesTokenCount": 4,
            "thoughtsTokenCount": 6,
            "cachedContentTokenCount": 3,
            "totalTokenCount": 20
        })))
        .unwrap();
        assert_eq!(usage.prompt_tokens, Some(10));
        assert_eq!(usage.completion_tokens, Some(4));
        assert_eq!(usage.reasoning_tokens, Some(6));
        assert_eq!(usage.cached_tokens, Some(3));
        assert_eq!(usage.total_tokens, Some(20));
        assert_eq!(normalize_gemini_usage(None), None);
        assert_eq!(normalize_gemini_usage(Some(&json!(1))), None);
    }

    #[test]
    fn folds_the_two_nullable_spellings() {
        assert_eq!(
            sanitize(json!({ "anyOf": [{ "type": "string", "enum": ["a"] }, { "type": "null" }] })),
            json!({ "type": "string", "enum": ["a"], "nullable": true })
        );
        assert_eq!(
            sanitize(json!({ "type": ["integer", "null"], "minimum": 1 })),
            json!({ "type": "integer", "minimum": 1, "nullable": true })
        );
    }

    #[test]
    fn schema_dialect_follows_the_model_family() {
        assert!(schema_dialect_for("gemini-3-flash").allow_any_of);
        assert!(!schema_dialect_for("claude-opus-4-6-thinking").allow_any_of);
        assert!(!schema_dialect_for("CLAUDE-OPUS").allow_any_of);
    }

    #[tokio::test]
    async fn splits_data_lines_across_awkward_chunk_boundaries() {
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(b"da")),
            Ok(Bytes::from_static(b"ta: {\"a\":1}\n\nda")),
            Ok(Bytes::from_static(b"ta: {\"b\":")),
            Ok(Bytes::from_static(b"2}\n\ndata: [DONE]\n\n")),
        ];
        let body: ByteStream = Box::pin(futures::stream::iter(chunks));
        let mut lines = SseDataLines::new(body);
        assert_eq!(lines.next_data().await.unwrap().unwrap(), "{\"a\":1}");
        assert_eq!(lines.next_data().await.unwrap().unwrap(), "{\"b\":2}");
        assert_eq!(lines.next_data().await.unwrap().unwrap(), "[DONE]");
        assert!(lines.next_data().await.is_none());
    }

    #[tokio::test]
    async fn yields_an_unterminated_trailing_line() {
        let chunks: Vec<Result<Bytes, std::io::Error>> =
            vec![Ok(Bytes::from_static(b"data: {\"a\":1}"))];
        let body: ByteStream = Box::pin(futures::stream::iter(chunks));
        let mut lines = SseDataLines::new(body);
        assert_eq!(lines.next_data().await.unwrap().unwrap(), "{\"a\":1}");
        assert!(lines.next_data().await.is_none());
    }

    #[tokio::test]
    async fn keeps_multibyte_text_split_across_chunks() {
        // "資" is three bytes; the split lands inside it.
        let raw = "data: {\"t\":\"資料\"}\n\n".as_bytes().to_vec();
        let (a, b) = raw.split_at(10);
        let chunks: Vec<Result<Bytes, std::io::Error>> =
            vec![Ok(Bytes::copy_from_slice(a)), Ok(Bytes::copy_from_slice(b))];
        let body: ByteStream = Box::pin(futures::stream::iter(chunks));
        let mut lines = SseDataLines::new(body);
        assert_eq!(lines.next_data().await.unwrap().unwrap(), "{\"t\":\"資料\"}");
    }
}
