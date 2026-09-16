//! Codex Responses SSE → OpenAI Chat Completions (//! docs/providers.md § Codex). The stream converter emits one Chat chunk per upstream
//! event; `response.failed` becomes a single OpenAI-shaped error line with no finish chunk
//! and no `[DONE]`, so a failure is never fabricated into a successful turn.
//!
//! Lines are split incrementally by [`crate::proxy::responses_openai::sse_line_stream`];
//! Rust streams are pull-based, so the TypeScript `backpressuredStream` wrapper has no
//! counterpart here.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::Arc;

use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::{json, Map, Value};

use crate::app::now_ms;
use crate::ids::new_id;
use crate::proxy::responses_openai::sse_line_stream;

/// Called once for a successful completed turn, without buffering the stream: the opaque
/// replayable output items and the assistant text of the turn.
pub type CodexReplayItemsCallback = Arc<dyn Fn(Vec<Value>, String) + Send + Sync>;

#[derive(Clone, Default)]
pub struct CodexSseOptions {
    pub on_replay_items: Option<CodexReplayItemsCallback>,
}

impl CodexSseOptions {
    pub fn with_replay_items(callback: CodexReplayItemsCallback) -> Self {
        Self { on_replay_items: Some(callback) }
    }
}

impl std::fmt::Debug for CodexSseOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexSseOptions")
            .field("on_replay_items", &self.on_replay_items.is_some())
            .finish()
    }
}

fn str_of(v: Option<&Value>) -> Option<&str> {
    v.and_then(|v| v.as_str())
}

fn obj_of(v: Option<&Value>) -> Option<&Map<String, Value>> {
    v.and_then(|v| v.as_object())
}

/// Pick the opaque replayable Responses output items, preserving upstream order.
pub fn extract_codex_replay_items(output: Option<&Value>) -> Vec<Value> {
    let Some(Value::Array(items)) = output else { return Vec::new() };
    items
        .iter()
        .filter(|item| {
            item.as_object()
                .map(|o| matches!(str_of(o.get("type")), Some("reasoning" | "function_call" | "custom_tool_call")))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn assistant_text_from_output(output: Option<&Value>) -> String {
    let Some(Value::Array(items)) = output else { return String::new() };
    let mut trailing = String::new();
    for item in items {
        let Some(record) = item.as_object() else { continue };
        if str_of(record.get("type")) != Some("message") || str_of(record.get("role")) != Some("assistant") {
            continue;
        }
        match record.get("content") {
            Some(Value::String(s)) => {
                trailing = s.clone();
            }
            Some(Value::Array(parts)) => {
                let mut text = String::new();
                for part in parts {
                    let Some(part) = part.as_object() else { continue };
                    if str_of(part.get("type")) == Some("output_text") {
                        if let Some(t) = str_of(part.get("text")) {
                            text.push_str(t);
                        }
                    }
                }
                if !text.is_empty() {
                    trailing = text;
                }
            }
            _ => {}
        }
    }
    trailing
}

fn replay_items_from_event(ev: &Map<String, Value>, assistant_text: &str, opts: &CodexSseOptions) {
    let Some(callback) = opts.on_replay_items.as_ref() else { return };
    let output = obj_of(ev.get("response")).and_then(|r| r.get("output"));
    let text = if assistant_text.is_empty() {
        assistant_text_from_output(output)
    } else {
        assistant_text.to_string()
    };
    // A replay tap must never break the upstream response.
    let items = extract_codex_replay_items(output);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(items, text)));
}

/// `ev.response?.error?.message`, `ev.error?.message`, `ev.message`, then a fallback.
fn codex_error_message(ev: &Map<String, Value>) -> String {
    obj_of(ev.get("response"))
        .and_then(|r| obj_of(r.get("error")))
        .and_then(|e| str_of(e.get("message")))
        .filter(|m| !m.is_empty())
        .or_else(|| obj_of(ev.get("error")).and_then(|e| str_of(e.get("message"))).filter(|m| !m.is_empty()))
        .or_else(|| str_of(ev.get("message")).filter(|m| !m.is_empty()))
        .unwrap_or("codex upstream failure")
        .to_string()
}

fn now_seconds() -> i64 {
    now_ms().div_euclid(1000)
}

/// Responses `usage` → Chat `usage`; `prompt_tokens_details` only when the Responses API
/// actually reported a cached-token count (absent means unreported, not zero).
fn chat_usage_from_responses(usage: &Value) -> Value {
    let u = usage.as_object().cloned().unwrap_or_default();
    let mut out = Map::new();
    out.insert("prompt_tokens".into(), json!(u.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0)));
    out.insert("completion_tokens".into(), json!(u.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0)));
    if let Some(cached) =
        obj_of(u.get("input_tokens_details")).and_then(|d| d.get("cached_tokens")).and_then(|v| v.as_i64())
    {
        out.insert("prompt_tokens_details".into(), json!({ "cached_tokens": cached }));
    }
    Value::Object(out)
}

struct ToolEntry {
    tool_index: i64,
    saw_args: bool,
}

struct CodexConverter {
    id: String,
    model: String,
    opts: CodexSseOptions,
    sent_role: bool,
    finished: bool,
    completed: bool,
    assistant_text: String,
    saw_tool_call: bool,
    next_tool_index: i64,
    /// Responses item id → chat tool_calls index + whether any args streamed.
    tools: HashMap<String, ToolEntry>,
    /// Argument deltas that arrived before their item's `added` event.
    pending_args: HashMap<String, String>,
    out: VecDeque<Bytes>,
}

impl CodexConverter {
    fn new(model: String, opts: CodexSseOptions) -> Self {
        let mut id = new_id("");
        id.truncate(24);
        Self {
            id: format!("chatcmpl_{id}"),
            model,
            opts,
            sent_role: false,
            finished: false,
            completed: false,
            assistant_text: String::new(),
            saw_tool_call: false,
            next_tool_index: 0,
            tools: HashMap::new(),
            pending_args: HashMap::new(),
            out: VecDeque::new(),
        }
    }

    fn chunk(&mut self, choice: Value, usage: Option<Value>) {
        let mut chunk = Map::new();
        chunk.insert("id".into(), json!(self.id));
        chunk.insert("object".into(), json!("chat.completion.chunk"));
        chunk.insert("created".into(), json!(now_seconds()));
        chunk.insert("model".into(), json!(self.model));
        let mut first = Map::new();
        first.insert("index".into(), json!(0));
        first.insert("finish_reason".into(), Value::Null);
        if let Some(choice) = choice.as_object() {
            for (k, v) in choice {
                first.insert(k.clone(), v.clone());
            }
        }
        chunk.insert("choices".into(), json!([Value::Object(first)]));
        if let Some(usage) = usage {
            chunk.insert("usage".into(), usage);
        }
        self.out.push_back(Bytes::from(format!("data: {}\n\n", Value::Object(chunk))));
    }

    fn ensure_role(&mut self) {
        if self.sent_role {
            return;
        }
        self.sent_role = true;
        self.chunk(json!({ "delta": { "role": "assistant", "content": "" } }), None);
    }

    fn emit_tool_args(&mut self, tool_index: i64, args: &str) {
        self.chunk(
            json!({ "delta": { "tool_calls": [{ "index": tool_index, "function": { "arguments": args } }] } }),
            None,
        );
    }

    /// Emits the tool header chunk and flushes any stashed arguments; returns the tool
    /// index and whether arguments have already been streamed for it.
    fn emit_tool_header(&mut self, item_id: &str, call_id: &str, name: &str) -> (i64, bool) {
        self.ensure_role();
        let tool_index = self.next_tool_index;
        self.next_tool_index += 1;
        self.tools.insert(item_id.to_string(), ToolEntry { tool_index, saw_args: false });
        self.saw_tool_call = true;
        self.chunk(
            json!({
                "delta": {
                    "tool_calls": [{
                        "index": tool_index,
                        "id": call_id,
                        "type": "function",
                        "function": { "name": name, "arguments": "" }
                    }]
                }
            }),
            None,
        );
        if let Some(stashed) = self.pending_args.remove(item_id).filter(|s| !s.is_empty()) {
            if let Some(entry) = self.tools.get_mut(item_id) {
                entry.saw_args = true;
            }
            self.emit_tool_args(tool_index, &stashed);
            return (tool_index, true);
        }
        (tool_index, false)
    }

    fn finish(&mut self, usage: Option<Value>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.ensure_role();
        let finish_reason = if self.saw_tool_call { "tool_calls" } else { "stop" };
        self.chunk(json!({ "delta": {}, "finish_reason": finish_reason }), usage);
        self.out.push_back(Bytes::from_static(b"data: [DONE]\n\n"));
    }

    /// `response.failed` / `error`: a single OpenAI-shaped error line, no finish chunk, no
    /// `[DONE]` — marks the stream finished so the trailing `finish()` is a no-op and no
    /// further events process.
    fn emit_error(&mut self, message: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        let payload = json!({ "error": { "message": message, "type": "upstream_error" } });
        self.out.push_back(Bytes::from(format!("data: {payload}\n\n")));
    }

    fn capture_completed(&mut self, ev: &Map<String, Value>) {
        if self.completed {
            return;
        }
        self.completed = true;
        let text = self.assistant_text.clone();
        replay_items_from_event(ev, &text, &self.opts);
    }

    fn on_line(&mut self, line: &str) {
        let Some(rest) = line.strip_prefix("data:") else { return };
        let data = rest.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        if self.finished {
            return;
        }
        let Ok(ev) = serde_json::from_str::<Value>(data) else { return };
        let Some(ev) = ev.as_object().cloned() else { return };
        let ty = str_of(ev.get("type")).unwrap_or("").to_string();
        let item = obj_of(ev.get("item")).cloned();
        let item_type = item.as_ref().and_then(|i| str_of(i.get("type"))).unwrap_or("").to_string();

        if ty == "response.output_text.delta" || ty == "response.reasoning_summary_text.delta" {
            let text = str_of(ev.get("delta")).unwrap_or("").to_string();
            if text.is_empty() {
                return;
            }
            self.ensure_role();
            if ty == "response.output_text.delta" {
                if self.opts.on_replay_items.is_some() {
                    self.assistant_text.push_str(&text);
                }
                self.chunk(json!({ "delta": { "content": text } }), None);
            } else {
                // De-facto extension field (DeepSeek/OpenRouter convention); OpenAI Chat
                // Completions has no first-party reasoning field.
                self.chunk(json!({ "delta": { "reasoning_content": text } }), None);
            }
        } else if ty == "response.output_item.added" && item_type == "function_call" {
            let item = item.expect("item present");
            // Key symmetric with the done handler so an id-less item still matches its own
            // done event via call_id.
            let item_id = str_of(item.get("id"))
                .filter(|s| !s.is_empty())
                .or_else(|| str_of(item.get("call_id")).filter(|s| !s.is_empty()))
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("item_{}", self.next_tool_index));
            if !self.tools.contains_key(&item_id) {
                let call_id = str_of(item.get("call_id")).filter(|s| !s.is_empty()).unwrap_or(&item_id).to_string();
                let name = str_of(item.get("name")).filter(|s| !s.is_empty()).unwrap_or("unknown").to_string();
                self.emit_tool_header(&item_id, &call_id, &name);
            }
        } else if ty == "response.function_call_arguments.delta" {
            let item_id = str_of(ev.get("item_id")).unwrap_or("").to_string();
            let delta = str_of(ev.get("delta")).unwrap_or("").to_string();
            if item_id.is_empty() || delta.is_empty() {
                return;
            }
            match self.tools.get_mut(&item_id) {
                Some(entry) => {
                    entry.saw_args = true;
                    let tool_index = entry.tool_index;
                    self.emit_tool_args(tool_index, &delta);
                }
                None => {
                    // `added` not seen yet; hold until the header can go out.
                    self.pending_args.entry(item_id).or_default().push_str(&delta);
                }
            }
        } else if ty == "response.output_item.done" && item_type == "function_call" {
            let item = item.expect("item present");
            let item_id = str_of(item.get("id"))
                .filter(|s| !s.is_empty())
                .or_else(|| str_of(item.get("call_id")).filter(|s| !s.is_empty()))
                .unwrap_or("")
                .to_string();
            let arguments = str_of(item.get("arguments")).unwrap_or("").to_string();
            let entry = if item_id.is_empty() { None } else { self.tools.get(&item_id).map(|e| e.tool_index) };
            match entry {
                None => {
                    // `added` never arrived. The done item's arguments are the backend's
                    // complete copy — a stash built from deltas may be missing pieces, so
                    // it yields to them.
                    if !item_id.is_empty() && !arguments.is_empty() {
                        self.pending_args.remove(&item_id);
                    }
                    let key = if item_id.is_empty() {
                        format!("item_{}", self.next_tool_index)
                    } else {
                        item_id.clone()
                    };
                    let call_id = str_of(item.get("call_id"))
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            if item_id.is_empty() {
                                format!("call_{}", self.next_tool_index)
                            } else {
                                item_id.clone()
                            }
                        });
                    let name = str_of(item.get("name")).filter(|s| !s.is_empty()).unwrap_or("unknown").to_string();
                    let (tool_index, saw_args) = self.emit_tool_header(&key, &call_id, &name);
                    if !saw_args && !arguments.is_empty() {
                        if let Some(entry) = self.tools.get_mut(&key) {
                            entry.saw_args = true;
                        }
                        self.emit_tool_args(tool_index, &arguments);
                    }
                }
                Some(tool_index) => {
                    let saw_args = self.tools.get(&item_id).map(|e| e.saw_args).unwrap_or(false);
                    if !saw_args && !arguments.is_empty() {
                        // Header went out but no deltas ever came.
                        if let Some(entry) = self.tools.get_mut(&item_id) {
                            entry.saw_args = true;
                        }
                        self.emit_tool_args(tool_index, &arguments);
                    }
                }
            }
        } else if ty == "response.completed" || ty == "response.done" {
            self.capture_completed(&ev);
            let usage = obj_of(ev.get("response")).and_then(|r| r.get("usage")).cloned();
            self.finish(usage.as_ref().map(chat_usage_from_responses));
        } else if ty == "response.failed" || ty == "error" {
            let message = codex_error_message(&ev);
            self.emit_error(&message);
        }
    }
}

/// Codex Responses SSE → OpenAI Chat Completions chunks.
pub fn codex_sse_to_openai_stream<S>(
    body: S,
    model: impl Into<String>,
    opts: CodexSseOptions,
) -> impl Stream<Item = Result<Bytes, io::Error>> + Send
where
    S: Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
{
    struct State<L> {
        lines: L,
        conv: CodexConverter,
        done: bool,
    }
    let state =
        State { lines: Box::pin(sse_line_stream(body)), conv: CodexConverter::new(model.into(), opts), done: false };
    futures::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(chunk) = st.conv.out.pop_front() {
                return Some((Ok(chunk), st));
            }
            if st.done {
                return None;
            }
            match st.lines.next().await {
                Some(Ok(line)) => st.conv.on_line(&line),
                Some(Err(e)) => {
                    st.done = true;
                    return Some((Err(e), st));
                }
                None => {
                    // Upstream ended without response.completed: terminate the stream
                    // properly so downstream consumers do not hang on a half-open turn.
                    // A no-op when emit_error() already finished the stream.
                    st.conv.finish(None);
                    st.done = true;
                }
            }
        }
    })
}

/// Discriminant used by callers to tell a real completion from an upstream failure.
#[derive(Debug, Clone, PartialEq)]
pub enum CodexCollected {
    Completion(Value),
    Error { message: String },
}

impl CodexCollected {
    /// `{"error":{"message":…,"type":"upstream_error"}}` for the caller's envelope.
    pub fn error_value(&self) -> Option<Value> {
        match self {
            CodexCollected::Error { message } => {
                Some(json!({ "error": { "message": message, "type": "upstream_error" } }))
            }
            CodexCollected::Completion(_) => None,
        }
    }
}

/// Non-stream codex path: drain a Responses SSE into one Chat completion object, or report
/// the upstream failure instead of fabricating a partial success.
pub async fn collect_codex_sse<S>(body: S, model: &str, opts: CodexSseOptions) -> CodexCollected
where
    S: Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
{
    let mut lines = Box::pin(sse_line_stream(body));
    let mut text = String::new();
    let mut reasoning_text = String::new();
    let mut completed = false;
    let mut usage: Option<Value> = None;
    let mut error: Option<String> = None;
    let mut tool_calls: Vec<Value> = Vec::new();

    while let Some(line) = lines.next().await {
        let Ok(line) = line else { break };
        let Some(rest) = line.strip_prefix("data:") else { continue };
        let data = rest.trim();
        if data.is_empty() {
            continue;
        }
        // Stop processing once a failure event lands: the turn is over, and a later
        // well-formed event must not overwrite the error with a fake partial success.
        if error.is_some() {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<Value>(data) else { continue };
        let Some(ev) = ev.as_object().cloned() else { continue };
        let ty = str_of(ev.get("type")).unwrap_or("").to_string();
        if ty == "response.failed" || ty == "error" {
            error = Some(codex_error_message(&ev));
            continue;
        }
        let delta = str_of(ev.get("delta")).unwrap_or("");
        if ty == "response.output_text.delta" && !delta.is_empty() {
            text.push_str(delta);
        }
        if ty == "response.reasoning_summary_text.delta" && !delta.is_empty() {
            reasoning_text.push_str(delta);
        }
        if ty == "response.output_item.done" {
            if let Some(item) = obj_of(ev.get("item")) {
                if str_of(item.get("type")) == Some("function_call") {
                    let mut call = Map::new();
                    call.insert("id".into(), item.get("call_id").cloned().unwrap_or(Value::Null));
                    call.insert("type".into(), json!("function"));
                    let mut function = Map::new();
                    function.insert("name".into(), item.get("name").cloned().unwrap_or(Value::Null));
                    function.insert(
                        "arguments".into(),
                        match item.get("arguments") {
                            Some(Value::Null) | None => json!("{}"),
                            Some(v) => v.clone(),
                        },
                    );
                    call.insert("function".into(), Value::Object(function));
                    tool_calls.push(Value::Object(call));
                }
            }
        }
        if ty == "response.completed" || ty == "response.done" {
            if !completed {
                completed = true;
                replay_items_from_event(&ev, &text, &opts);
            }
            if let Some(u) = obj_of(ev.get("response")).and_then(|r| r.get("usage")) {
                usage = Some(u.clone());
            }
        }
    }

    if let Some(message) = error {
        return CodexCollected::Error { message };
    }

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), if text.is_empty() { Value::Null } else { json!(text) });
    if !reasoning_text.is_empty() {
        // De-facto extension field (DeepSeek/OpenRouter convention).
        message.insert("reasoning_content".into(), json!(reasoning_text));
    }
    let has_tool_calls = !tool_calls.is_empty();
    if has_tool_calls {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    let mut choice = Map::new();
    choice.insert("index".into(), json!(0));
    choice.insert("message".into(), Value::Object(message));
    choice.insert("finish_reason".into(), json!(if has_tool_calls { "tool_calls" } else { "stop" }));

    let mut completion = Map::new();
    completion.insert("id".into(), json!(format!("chatcmpl_{}", now_ms())));
    completion.insert("object".into(), json!("chat.completion"));
    completion.insert("created".into(), json!(now_seconds()));
    completion.insert("model".into(), json!(model));
    completion.insert("choices".into(), json!([Value::Object(choice)]));
    if let Some(usage) = usage {
        completion.insert("usage".into(), chat_usage_from_responses(&usage));
    }
    CodexCollected::Completion(Value::Object(completion))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn chunked(text: &str, size: usize) -> impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static {
        let bytes = Bytes::from(text.to_string());
        let chunks: Vec<Result<Bytes, io::Error>> =
            (0..bytes.len()).step_by(size).map(|i| Ok(bytes.slice(i..(i + size).min(bytes.len())))).collect();
        futures::stream::iter(chunks)
    }

    async fn collect_text<S>(stream: S) -> String
    where
        S: Stream<Item = Result<Bytes, io::Error>>,
    {
        let mut out = Vec::new();
        let mut stream = Box::pin(stream);
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("no stream error"));
        }
        String::from_utf8(out).expect("utf-8")
    }

    fn d(value: Value) -> String {
        format!("data: {value}")
    }

    #[derive(Debug, Default)]
    struct ParsedChunks {
        text: String,
        reasoning: String,
        tools: Vec<(i64, Option<String>, Option<String>, String)>,
        finish: Option<String>,
        usage: Option<Value>,
        done_count: usize,
    }

    /// Reassemble an OpenAI chunk stream: text, reasoning, tool_calls by index, finish, usage.
    fn parse_chunks(sse: &str) -> ParsedChunks {
        let mut tools: Vec<(i64, Option<String>, Option<String>, String)> = Vec::new();
        let mut out = ParsedChunks::default();
        let mut role_count = 0;
        let mut chunk_count = 0;
        for line in sse.split('\n') {
            let Some(rest) = line.strip_prefix("data:") else { continue };
            let data = rest.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                out.done_count += 1;
                continue;
            }
            let j: Value = serde_json::from_str(data).expect("chunk JSON");
            chunk_count += 1;
            if let Some(usage) = j.get("usage") {
                out.usage = Some(usage.clone());
            }
            let Some(choice) = j.get("choices").and_then(|c| c.as_array()).and_then(|c| c.first()) else {
                continue;
            };
            let delta = choice.get("delta");
            if delta.and_then(|d| d.get("role")).is_some() {
                role_count += 1;
                assert_eq!(chunk_count, 1, "role chunk must come first");
            }
            if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                assert!(out.finish.is_none(), "finish_reason emitted twice");
                out.finish = Some(reason.to_string());
            }
            if let Some(content) = delta.and_then(|d| d.get("content")).and_then(|v| v.as_str()) {
                out.text.push_str(content);
            }
            if let Some(reasoning) = delta.and_then(|d| d.get("reasoning_content")).and_then(|v| v.as_str()) {
                out.reasoning.push_str(reasoning);
            }
            let calls = delta.and_then(|d| d.get("tool_calls")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
            for tc in calls {
                let index = tc.get("index").and_then(|v| v.as_i64()).expect("tool index");
                let slot = match tools.iter_mut().find(|t| t.0 == index) {
                    Some(slot) => slot,
                    None => {
                        tools.push((index, None, None, String::new()));
                        tools.last_mut().expect("just pushed")
                    }
                };
                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    slot.1 = Some(id.to_string());
                }
                if let Some(name) = tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        slot.2 = Some(name.to_string());
                    }
                }
                if let Some(args) = tc.get("function").and_then(|f| f.get("arguments")).and_then(|v| v.as_str()) {
                    slot.3.push_str(args);
                }
            }
        }
        assert_eq!(role_count, 1, "assistant role chunk must appear exactly once");
        tools.sort_by_key(|t| t.0);
        out.tools = tools;
        out
    }

    fn tool_call_events() -> Vec<String> {
        vec![
            d(json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Read", "arguments": "" }
            })),
            String::new(),
            d(json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_1",
                "output_index": 0,
                "delta": "{\"file_path\":"
            })),
            String::new(),
            d(json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_1",
                "output_index": 0,
                "delta": "\"/repo/package.json\"}"
            })),
            String::new(),
            d(json!({
                "type": "response.function_call_arguments.done",
                "item_id": "fc_1",
                "arguments": "{\"file_path\":\"/repo/package.json\"}"
            })),
            String::new(),
            d(json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "Read",
                    "arguments": "{\"file_path\":\"/repo/package.json\"}"
                }
            })),
            String::new(),
            d(json!({ "type": "response.completed", "response": { "usage": { "input_tokens": 120, "output_tokens": 18 } } })),
            String::new(),
        ]
    }

    fn tool_call_sse() -> String {
        tool_call_events().join("\n")
    }

    /// Same tool round, preceded by a reasoning summary that streams first.
    fn tool_call_with_reasoning_sse() -> String {
        let mut lines = vec![
            d(json!({ "type": "response.reasoning_summary_text.delta", "delta": "Let me check " })),
            String::new(),
            d(json!({ "type": "response.reasoning_summary_text.delta", "delta": "the file first." })),
            String::new(),
        ];
        lines.extend(tool_call_events());
        lines.join("\n")
    }

    #[tokio::test]
    async fn streams_text_and_finishes_with_stop() {
        let sse = [
            d(json!({ "type": "response.output_text.delta", "delta": "hel" })),
            String::new(),
            d(json!({ "type": "response.output_text.delta", "delta": "lo" })),
            String::new(),
            d(json!({ "type": "response.completed", "response": {} })),
            String::new(),
        ]
        .join("\n");
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&sse, 9), "gpt-5.2", CodexSseOptions::default())).await,
        );
        assert_eq!(out.text, "hello");
        assert!(out.tools.is_empty());
        assert_eq!(out.finish.as_deref(), Some("stop"));
        assert_eq!(out.done_count, 1);
    }

    #[tokio::test]
    async fn maps_cached_tokens_onto_prompt_tokens_details() {
        let sse = [
            d(json!({ "type": "response.output_text.delta", "delta": "hi" })),
            String::new(),
            d(json!({
                "type": "response.completed",
                "response": { "usage": { "input_tokens": 500, "output_tokens": 20, "input_tokens_details": { "cached_tokens": 200 } } }
            })),
            String::new(),
        ]
        .join("\n");
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&sse, 15), "gpt-5.2", CodexSseOptions::default())).await,
        );
        assert_eq!(
            out.usage,
            Some(json!({ "prompt_tokens": 500, "completion_tokens": 20, "prompt_tokens_details": { "cached_tokens": 200 } }))
        );
    }

    #[tokio::test]
    async fn omits_prompt_tokens_details_without_cache_detail() {
        let sse = [
            d(json!({ "type": "response.output_text.delta", "delta": "hi" })),
            String::new(),
            d(json!({ "type": "response.completed", "response": { "usage": { "input_tokens": 5, "output_tokens": 1 } } })),
            String::new(),
        ]
        .join("\n");
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&sse, 15), "gpt-5.2", CodexSseOptions::default())).await,
        );
        assert_eq!(out.usage, Some(json!({ "prompt_tokens": 5, "completion_tokens": 1 })));
    }

    #[tokio::test]
    async fn maps_a_streamed_function_call_to_tool_call_chunks() {
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&tool_call_sse(), 13), "gpt-5.2", CodexSseOptions::default()))
                .await,
        );
        assert_eq!(out.tools.len(), 1);
        assert_eq!(out.tools[0].1.as_deref(), Some("call_1"));
        assert_eq!(out.tools[0].2.as_deref(), Some("Read"));
        // Deltas streamed once; arguments.done and item.done must not re-append.
        assert_eq!(
            serde_json::from_str::<Value>(&out.tools[0].3).expect("args JSON"),
            json!({ "file_path": "/repo/package.json" })
        );
        assert_eq!(out.finish.as_deref(), Some("tool_calls"));
        assert_eq!(out.usage, Some(json!({ "prompt_tokens": 120, "completion_tokens": 18 })));
        assert_eq!(out.done_count, 1);
    }

    #[tokio::test]
    async fn falls_back_to_the_done_item_when_no_deltas_were_sent() {
        let sse = [
            d(json!({
                "type": "response.output_item.added",
                "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Bash", "arguments": "" }
            })),
            String::new(),
            d(json!({
                "type": "response.output_item.done",
                "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Bash", "arguments": "{\"command\":\"ls\"}" }
            })),
            String::new(),
            d(json!({ "type": "response.completed", "response": {} })),
            String::new(),
        ]
        .join("\n");
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&sse, 17), "m", CodexSseOptions::default())).await,
        );
        assert_eq!(out.tools.len(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&out.tools[0].3).expect("args JSON"),
            json!({ "command": "ls" })
        );
        assert_eq!(out.finish.as_deref(), Some("tool_calls"));
    }

    #[tokio::test]
    async fn recovers_a_call_whose_added_event_was_missed() {
        let sse = [
            d(json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_9", "delta": "{\"a\":" })),
            String::new(),
            d(json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_9", "delta": "1}" })),
            String::new(),
            d(json!({
                "type": "response.output_item.done",
                "item": { "type": "function_call", "id": "fc_9", "call_id": "call_9", "name": "T", "arguments": "{\"a\":1}" }
            })),
            String::new(),
            d(json!({ "type": "response.completed", "response": {} })),
            String::new(),
        ]
        .join("\n");
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&sse, 11), "m", CodexSseOptions::default())).await,
        );
        assert_eq!(out.tools.len(), 1);
        assert_eq!(out.tools[0].1.as_deref(), Some("call_9"));
        assert_eq!(out.tools[0].2.as_deref(), Some("T"));
        // Stashed deltas flush once; the done item must not append a second copy.
        assert_eq!(serde_json::from_str::<Value>(&out.tools[0].3).expect("args JSON"), json!({ "a": 1 }));
    }

    #[tokio::test]
    async fn keeps_two_calls_on_distinct_tool_indices() {
        let sse = [
            d(json!({
                "type": "response.output_item.added",
                "item": { "type": "function_call", "id": "fc_1", "call_id": "call_a", "name": "A", "arguments": "" }
            })),
            String::new(),
            d(json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{\"a\":1}" })),
            String::new(),
            d(json!({
                "type": "response.output_item.added",
                "item": { "type": "function_call", "id": "fc_2", "call_id": "call_b", "name": "B", "arguments": "" }
            })),
            String::new(),
            d(json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_2", "delta": "{\"b\":2}" })),
            String::new(),
            d(json!({ "type": "response.completed", "response": {} })),
            String::new(),
        ]
        .join("\n");
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&sse, 19), "m", CodexSseOptions::default())).await,
        );
        let summary: Vec<(i64, Option<String>, Option<String>)> =
            out.tools.iter().map(|t| (t.0, t.1.clone(), t.2.clone())).collect();
        assert_eq!(
            summary,
            vec![
                (0, Some("call_a".to_string()), Some("A".to_string())),
                (1, Some("call_b".to_string()), Some("B".to_string())),
            ]
        );
        let args: Vec<Value> =
            out.tools.iter().map(|t| serde_json::from_str::<Value>(&t.3).expect("args JSON")).collect();
        assert_eq!(args, vec![json!({ "a": 1 }), json!({ "b": 2 })]);
    }

    #[tokio::test]
    async fn ignores_events_after_response_completed() {
        let sse = [
            d(json!({ "type": "response.output_text.delta", "delta": "done" })),
            String::new(),
            d(json!({ "type": "response.completed", "response": {} })),
            String::new(),
            d(json!({ "type": "response.output_text.delta", "delta": "late" })),
            String::new(),
            d(json!({ "type": "response.completed", "response": {} })),
            String::new(),
        ]
        .join("\n");
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&sse, 21), "m", CodexSseOptions::default())).await,
        );
        assert_eq!(out.text, "done");
        assert_eq!(out.finish.as_deref(), Some("stop"));
        assert_eq!(out.done_count, 1);
    }

    #[tokio::test]
    async fn uses_stashed_deltas_when_the_done_item_has_no_arguments() {
        // pendingArgs is the only source here: `added` was missed and the done item's
        // arguments are empty.
        let sse = [
            d(json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_9", "delta": "{\"a\":" })),
            String::new(),
            d(json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_9", "delta": "1}" })),
            String::new(),
            d(json!({
                "type": "response.output_item.done",
                "item": { "type": "function_call", "id": "fc_9", "call_id": "call_9", "name": "T", "arguments": "" }
            })),
            String::new(),
            d(json!({ "type": "response.completed", "response": {} })),
            String::new(),
        ]
        .join("\n");
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&sse, 11), "m", CodexSseOptions::default())).await,
        );
        assert_eq!(out.tools.len(), 1);
        assert_eq!(serde_json::from_str::<Value>(&out.tools[0].3).expect("args JSON"), json!({ "a": 1 }));
    }

    #[tokio::test]
    async fn prefers_the_done_items_full_arguments_over_a_partial_stash() {
        // `added` missed and one delta lost: the stash holds broken JSON, the done item
        // holds the backend's complete copy.
        let sse = [
            d(json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_9", "delta": "{\"a\":" })),
            String::new(),
            d(json!({
                "type": "response.output_item.done",
                "item": { "type": "function_call", "id": "fc_9", "call_id": "call_9", "name": "T", "arguments": "{\"a\":1}" }
            })),
            String::new(),
            d(json!({ "type": "response.completed", "response": {} })),
            String::new(),
        ]
        .join("\n");
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&sse, 15), "m", CodexSseOptions::default())).await,
        );
        assert_eq!(out.tools.len(), 1);
        assert_eq!(serde_json::from_str::<Value>(&out.tools[0].3).expect("args JSON"), json!({ "a": 1 }));
    }

    #[tokio::test]
    async fn terminates_when_upstream_ends_without_response_completed() {
        let sse = [d(json!({ "type": "response.output_text.delta", "delta": "hi" })), String::new()].join("\n");
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&sse, 7), "m", CodexSseOptions::default())).await,
        );
        assert_eq!(out.text, "hi");
        assert_eq!(out.finish.as_deref(), Some("stop"));
        assert_eq!(out.done_count, 1);
    }

    #[tokio::test]
    async fn streams_reasoning_summary_as_reasoning_content() {
        let sse = [
            d(json!({ "type": "response.reasoning_summary_text.delta", "delta": "thinking " })),
            String::new(),
            d(json!({ "type": "response.reasoning_summary_text.delta", "delta": "hard" })),
            String::new(),
            d(json!({ "type": "response.output_text.delta", "delta": "answer" })),
            String::new(),
            d(json!({ "type": "response.completed", "response": {} })),
            String::new(),
        ]
        .join("\n");
        let out = parse_chunks(
            &collect_text(codex_sse_to_openai_stream(chunked(&sse, 10), "gpt-5.2", CodexSseOptions::default())).await,
        );
        assert_eq!(out.reasoning, "thinking hard");
        assert_eq!(out.text, "answer");
        assert_eq!(out.finish.as_deref(), Some("stop"));
    }

    #[tokio::test]
    async fn emits_a_single_error_line_on_response_failed() {
        let sse = [
            d(json!({ "type": "response.output_text.delta", "delta": "partial" })),
            String::new(),
            d(json!({ "type": "response.failed", "response": { "error": { "message": "rate limited, try again" } } })),
            String::new(),
        ]
        .join("\n");
        let out =
            collect_text(codex_sse_to_openai_stream(chunked(&sse, 11), "gpt-5.2", CodexSseOptions::default())).await;
        assert!(out.contains("data: {\"error\":{\"message\":\"rate limited, try again\",\"type\":\"upstream_error\"}}"));
        assert!(!out.contains("[DONE]"));
        assert!(!out.contains("\"finish_reason\":\"stop\""));
        assert!(!out.contains("\"finish_reason\":\"tool_calls\""));
    }

    #[tokio::test]
    async fn stops_processing_further_events_once_failed_lands() {
        let sse = [
            d(json!({ "type": "response.failed", "error": { "message": "boom" } })),
            String::new(),
            d(json!({ "type": "response.output_text.delta", "delta": "should not appear" })),
            String::new(),
            d(json!({ "type": "response.completed", "response": {} })),
            String::new(),
        ]
        .join("\n");
        let out = collect_text(codex_sse_to_openai_stream(chunked(&sse, 9), "m", CodexSseOptions::default())).await;
        assert!(!out.contains("should not appear"));
        assert!(!out.contains("[DONE]"));
        assert_eq!(out.split("\n\n").filter(|l| l.contains("\"error\"")).count(), 1);
    }

    #[tokio::test]
    async fn reports_replay_items_once_for_a_completed_turn() {
        type Captured = Vec<(Vec<Value>, String)>;
        let seen: Arc<Mutex<Captured>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let opts = CodexSseOptions::with_replay_items(Arc::new(move |items, text| {
            sink.lock().expect("lock").push((items, text));
        }));
        let sse = [
            d(json!({ "type": "response.output_text.delta", "delta": "hi" })),
            String::new(),
            d(json!({
                "type": "response.completed",
                "response": { "output": [
                    { "type": "reasoning", "id": "rs_1", "encrypted_content": "gAAAA" },
                    { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "hi" }] }
                ] }
            })),
            String::new(),
        ]
        .join("\n");
        let _ = collect_text(codex_sse_to_openai_stream(chunked(&sse, 13), "m", opts)).await;
        let captured = seen.lock().expect("lock").clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, vec![json!({ "type": "reasoning", "id": "rs_1", "encrypted_content": "gAAAA" })]);
        assert_eq!(captured[0].1, "hi");
    }

    async fn collect_ok(sse: &str, size: usize, model: &str) -> Value {
        match collect_codex_sse(chunked(sse, size), model, CodexSseOptions::default()).await {
            CodexCollected::Completion(v) => v,
            other => panic!("unexpected error result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn collect_captures_usage_from_response_completed() {
        let completion = collect_ok(&tool_call_sse(), 23, "gpt-5.2").await;
        assert_eq!(completion["usage"], json!({ "prompt_tokens": 120, "completion_tokens": 18 }));
        assert_eq!(completion["choices"][0]["finish_reason"], json!("tool_calls"));
    }

    #[tokio::test]
    async fn collect_maps_cached_tokens() {
        let sse = [
            d(json!({ "type": "response.output_text.delta", "delta": "hi" })),
            String::new(),
            d(json!({
                "type": "response.completed",
                "response": { "usage": { "input_tokens": 500, "output_tokens": 20, "input_tokens_details": { "cached_tokens": 200 } } }
            })),
            String::new(),
        ]
        .join("\n");
        let completion = collect_ok(&sse, 27, "gpt-5.2").await;
        assert_eq!(
            completion["usage"],
            json!({ "prompt_tokens": 500, "completion_tokens": 20, "prompt_tokens_details": { "cached_tokens": 200 } })
        );
    }

    #[tokio::test]
    async fn collect_omits_prompt_tokens_details_without_cache_detail() {
        let sse = [
            d(json!({ "type": "response.output_text.delta", "delta": "hi" })),
            String::new(),
            d(json!({ "type": "response.completed", "response": { "usage": { "input_tokens": 5, "output_tokens": 1 } } })),
            String::new(),
        ]
        .join("\n");
        let completion = collect_ok(&sse, 27, "gpt-5.2").await;
        assert_eq!(completion["usage"], json!({ "prompt_tokens": 5, "completion_tokens": 1 }));
    }

    #[tokio::test]
    async fn collect_omits_reasoning_content_when_none_streamed() {
        let completion = collect_ok(&tool_call_sse(), 23, "gpt-5.2").await;
        assert!(completion["choices"][0]["message"].get("reasoning_content").is_none());
    }

    #[tokio::test]
    async fn collect_sets_reasoning_content_from_the_summary() {
        let completion = collect_ok(&tool_call_with_reasoning_sse(), 31, "gpt-5.2").await;
        let message = completion["choices"][0]["message"].clone();
        assert_eq!(message["reasoning_content"], json!("Let me check the file first."));
        // Tool round itself is unaffected by the leading reasoning summary.
        assert_eq!(message["content"], Value::Null);
        assert_eq!(completion["choices"][0]["finish_reason"], json!("tool_calls"));
    }

    #[tokio::test]
    async fn collect_returns_an_error_marker_on_response_failed() {
        let sse = [
            d(json!({ "type": "response.failed", "response": { "error": { "message": "rate limited" } } })),
            String::new(),
        ]
        .join("\n");
        let result = collect_codex_sse(chunked(&sse, 9), "gpt-5.2", CodexSseOptions::default()).await;
        assert_eq!(result, CodexCollected::Error { message: "rate limited".into() });
        assert_eq!(
            result.error_value(),
            Some(json!({ "error": { "message": "rate limited", "type": "upstream_error" } }))
        );
    }

    #[tokio::test]
    async fn collect_falls_back_through_error_message_then_default() {
        let via_error = collect_codex_sse(
            chunked(&format!("{}\n", d(json!({ "type": "error", "error": { "message": "bad request" } }))), 7),
            "m",
            CodexSseOptions::default(),
        )
        .await;
        assert_eq!(via_error, CodexCollected::Error { message: "bad request".into() });

        let via_message = collect_codex_sse(
            chunked(&format!("{}\n", d(json!({ "type": "error", "message": "top-level message" }))), 7),
            "m",
            CodexSseOptions::default(),
        )
        .await;
        assert_eq!(via_message, CodexCollected::Error { message: "top-level message".into() });

        let via_default = collect_codex_sse(
            chunked(&format!("{}\n", d(json!({ "type": "error" }))), 7),
            "m",
            CodexSseOptions::default(),
        )
        .await;
        assert_eq!(via_default, CodexCollected::Error { message: "codex upstream failure".into() });
    }

    #[tokio::test]
    async fn collect_stops_accumulating_once_a_failure_lands() {
        let sse = [
            d(json!({ "type": "response.output_text.delta", "delta": "partial" })),
            String::new(),
            d(json!({ "type": "response.failed", "error": { "message": "boom" } })),
            String::new(),
            d(json!({ "type": "response.completed", "response": { "usage": { "input_tokens": 1, "output_tokens": 1 } } })),
            String::new(),
        ]
        .join("\n");
        let result = collect_codex_sse(chunked(&sse, 13), "m", CodexSseOptions::default()).await;
        assert_eq!(result, CodexCollected::Error { message: "boom".into() });
    }

    #[test]
    fn extracts_only_replayable_items() {
        let output = json!([
            { "type": "reasoning", "id": "rs_1" },
            { "type": "message", "role": "assistant", "content": [] },
            { "type": "function_call", "call_id": "c" },
            { "type": "custom_tool_call", "call_id": "d" },
            "not an object",
            { "type": "web_search_call" }
        ]);
        let items = extract_codex_replay_items(Some(&output));
        assert_eq!(
            items,
            vec![
                json!({ "type": "reasoning", "id": "rs_1" }),
                json!({ "type": "function_call", "call_id": "c" }),
                json!({ "type": "custom_tool_call", "call_id": "d" }),
            ]
        );
        assert!(extract_codex_replay_items(None).is_empty());
    }
}
