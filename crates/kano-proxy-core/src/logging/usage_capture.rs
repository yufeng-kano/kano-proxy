//! Port of apps/api/src/logging/usage_capture.ts (docs/logging.md § Token usage capture).
//!
//! Normalizes provider-shaped `usage` objects into `request_logs` columns, and incrementally
//! captures usage from an SSE body without buffering the stream itself — only a small bounded
//! partial-line carry.
//!
//! `None` means "unreported", not zero — see the token semantics in docs/database.md.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};

/// `request_logs` token columns. `prompt_tokens` is the cache-inclusive total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NormalizedUsage {
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
}

/// `NULL_USAGE`: nothing was reported.
pub const NULL_USAGE: NormalizedUsage = NormalizedUsage {
    prompt_tokens: None,
    completion_tokens: None,
    cache_read_input_tokens: None,
    cache_creation_input_tokens: None,
};

/// `num(v)`: a JSON number, or `None` for anything else (including a missing key).
fn num(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

fn field<'a>(obj: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    obj?.get(key)
}

/// OpenAI Chat Completions-shaped `usage` — also the shape this proxy's own converters build
/// for claude-code / custom-anthropic / codex on `/openai/v1`. OpenAI-compatible upstreams can
/// report cache writes in `prompt_tokens_details.cache_write_tokens`; converted responses
/// retain the proxy's `cache_creation_input_tokens` extension. `prompt_tokens` is already
/// cache-inclusive, so it is stored as-is, and `completion_tokens` is already total output
/// inclusive of reasoning tokens. A missing detail field means unreported: NULL, never 0.
pub fn from_openai_usage(usage: Option<&Value>) -> NormalizedUsage {
    let Some(u) = usage.filter(|v| !v.is_null()) else { return NULL_USAGE };
    let details = u.get("prompt_tokens_details");
    // Responses API shape (native `/openai/v1/responses` path): the cached count sits under
    // `input_tokens_details` instead.
    let input_details = u.get("input_tokens_details");
    let prompt = num(u.get("prompt_tokens")).or_else(|| num(u.get("input_tokens")));
    let completion = num(u.get("completion_tokens")).or_else(|| num(u.get("output_tokens")));
    NormalizedUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        cache_read_input_tokens: num(field(details, "cached_tokens"))
            .or_else(|| num(field(input_details, "cached_tokens"))),
        cache_creation_input_tokens: num(field(details, "cache_write_tokens"))
            .or_else(|| num(u.get("cache_creation_input_tokens"))),
    }
}

/// Anthropic Messages-shaped `usage`. `input_tokens` excludes cache reads and writes, so
/// `prompt_tokens` sums all three into the normalized *total* this proxy stores. Anthropic
/// always defines the cache fields on a real usage object, so a missing one there defaults to
/// 0 — only a wholly absent usage object means unreported (NULL).
pub fn from_anthropic_usage(usage: Option<&Value>) -> NormalizedUsage {
    let Some(u) = usage.filter(|v| !v.is_null()) else { return NULL_USAGE };
    let cache_read = num(u.get("cache_read_input_tokens")).unwrap_or(0);
    let cache_creation = num(u.get("cache_creation_input_tokens")).unwrap_or(0);
    NormalizedUsage {
        prompt_tokens: Some(num(u.get("input_tokens")).unwrap_or(0) + cache_read + cache_creation),
        completion_tokens: num(u.get("output_tokens")),
        cache_read_input_tokens: Some(cache_read),
        cache_creation_input_tokens: Some(cache_creation),
    }
}

/// Longest partial line kept in memory. A line that outgrows it is skipped, never buffered —
/// capture resumes on the next line. (Bytes here, UTF-16 units in the TypeScript; the cap is a
/// memory bound, not a contract.)
const MAX_CARRY: usize = 256 * 1024;
/// Longest `usage` object scanned out of an over-long Responses terminal line.
const MAX_USAGE_SCAN: usize = 4 * 1024;

/// A Responses SSE terminal event, as it starts a `data:` line.
static RESPONSES_TERMINAL_PREFIX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"^data:\s*\{\s*"type"\s*:\s*"response\.(completed|incomplete)""#).expect("static regex")
});

pub trait UsageSniffer: Send {
    /// Never fails — a bad chunk degrades capture, it never fails the request.
    fn feed(&mut self, chunk: &[u8]);
    /// `None` when nothing usable was captured (no usage seen, or carry overflow).
    fn finish(&self) -> Option<NormalizedUsage>;
    /// Whether the stream reached its documented completion signal (Anthropic `message_stop`;
    /// OpenAI `[DONE]` or a chunk carrying a non-null `finish_reason`; Responses
    /// `response.completed` / `response.incomplete`) at any point before this was called.
    fn complete(&self) -> bool;
}

/// Streaming UTF-8 decode, replacing `TextDecoder({stream:true})`: a sequence split across
/// chunks is completed on the next feed, and a genuinely invalid byte becomes U+FFFD.
#[derive(Default)]
struct Utf8Decoder {
    tail: Vec<u8>,
}

impl Utf8Decoder {
    fn decode(&mut self, chunk: &[u8]) -> String {
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(chunk);
        let mut out = String::with_capacity(buf.len());
        let mut offset = 0usize;
        loop {
            match std::str::from_utf8(&buf[offset..]) {
                Ok(text) => {
                    out.push_str(text);
                    break;
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    out.push_str(std::str::from_utf8(&buf[offset..offset + valid]).expect("valid prefix"));
                    match err.error_len() {
                        Some(len) => {
                            out.push('\u{FFFD}');
                            offset += valid + len;
                        }
                        None => {
                            self.tail = buf[offset + valid..].to_vec();
                            break;
                        }
                    }
                }
            }
        }
        out
    }
}

/// The last `n` bytes of `s`, snapped up to a character boundary (`s.slice(-n)`).
fn tail_str(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut start = s.len() - n;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// The first `n` bytes of `s`, snapped down to a character boundary (`s.slice(0, n)`).
fn head_str(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Index just past the JSON object starting at `start` (after optional whitespace): `-1` when
/// the object has not fully arrived yet, `-2` when the value is not an object at all
/// (`"usage":null`) or runs past `MAX_USAGE_SCAN`.
fn json_object_end(s: &str, start: usize) -> i64 {
    let b = s.as_bytes();
    let mut i = start;
    while i < b.len() && matches!(b[i], b' ' | b'\n' | b'\r' | b'\t') {
        i += 1;
    }
    if i >= b.len() {
        return -1;
    }
    if b[i] != b'{' {
        return -2;
    }
    let mut depth = 0i64;
    let mut in_string = false;
    while i < b.len() {
        if i - start > MAX_USAGE_SCAN {
            return -2;
        }
        let c = b[i];
        if in_string {
            if c == b'\\' {
                i += 1;
            } else if c == b'"' {
                in_string = false;
            }
        } else if c == b'"' {
            in_string = true;
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return (i + 1) as i64;
            }
        }
        i += 1;
    }
    -1
}

/// An over-long line in progress. Skipped, unless it is a Responses terminal event: the
/// ChatGPT codex backend echoes enough of the request into `response.completed` that a real
/// Codex CLI turn passes MAX_CARRY, so its small `usage` object is scanned out of the passing
/// bytes rather than the line being buffered.
struct LongLine {
    terminal: bool,
    window: String,
    found: Option<Value>,
}

/// `chat.completion.chunk` SSE, and the Responses SSE relayed by the native
/// `/openai/v1/responses` path. Usage rides on whichever chunk carries a non-null top-level
/// `usage` — normally only the final one, but the whole object is replaced last-wins if more
/// than one ever does — or on the terminal `response.completed` / `response.incomplete` event.
#[derive(Default)]
pub struct OpenAiSseUsageSniffer {
    decoder: Utf8Decoder,
    carry: String,
    usage: Option<Value>,
    /// `[DONE]`, a chunk whose `choices[].finish_reason` was a real (non-null) string, or a
    /// Responses terminal event.
    seen_completion: bool,
    /// Name from the last `event:` line — the Responses SSE labels its terminal event there.
    event_name: String,
    long_line: Option<LongLine>,
}

pub fn create_openai_sse_usage_sniffer() -> Box<dyn UsageSniffer> {
    Box::new(OpenAiSseUsageSniffer::default())
}

impl OpenAiSseUsageSniffer {
    fn process_line(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("event:") {
            self.event_name = rest.trim().to_string();
            return;
        }
        let Some(rest) = line.strip_prefix("data:") else { return };
        self.event_name = String::new();
        let data = rest.trim();
        if data.is_empty() {
            return;
        }
        if data == "[DONE]" {
            self.seen_completion = true;
            return;
        }
        // Cheap pre-filter before parsing — every other SSE line (a plain content/tool_calls
        // delta) is skipped without ever being parsed.
        if !data.contains("\"usage\"")
            && !data.contains("\"finish_reason\"")
            && !data.contains("\"response.completed\"")
            && !data.contains("\"response.incomplete\"")
        {
            return;
        }
        let Ok(json) = serde_json::from_str::<Value>(data) else { return };
        if !json.is_object() {
            return;
        }
        // Responses SSE relayed by the native `/openai/v1/responses` path: the terminal event
        // carries the usage and is the completion signal.
        let event_type = json.get("type").and_then(Value::as_str);
        if event_type == Some("response.completed") || event_type == Some("response.incomplete") {
            self.seen_completion = true;
            if let Some(usage) = json.get("response").and_then(|r| r.get("usage")).filter(|u| u.is_object()) {
                self.usage = Some(usage.clone());
            }
            return;
        }
        if let Some(usage) = json.get("usage").filter(|u| u.is_object()) {
            self.usage = Some(usage.clone());
        }
        if let Some(choices) = json.get("choices").and_then(Value::as_array) {
            for choice in choices {
                if choice.get("finish_reason").and_then(Value::as_str).is_some() {
                    self.seen_completion = true;
                    break;
                }
            }
        }
    }

    fn is_terminal_line(&self, line_start: &str) -> bool {
        RESPONSES_TERMINAL_PREFIX.is_match(head_str(line_start, 128))
            || self.event_name == "response.completed"
            || self.event_name == "response.incomplete"
    }

    /// Scan a slice of an over-long terminal line for its `usage` object; the last valid one wins.
    fn scan_long(&mut self, text: &str) {
        let Some(long) = self.long_line.as_mut() else { return };
        if !long.terminal {
            return;
        }
        let mut buf = String::with_capacity(long.window.len() + text.len());
        buf.push_str(&long.window);
        buf.push_str(text);
        let mut base = 0usize;
        loop {
            let slice = &buf[base..];
            let Some(offset) = slice.find("\"usage\":") else {
                // Keep just enough tail to complete a marker split across chunks.
                long.window = tail_str(slice, 7).to_string();
                return;
            };
            let idx = base + offset;
            let start = idx + 8;
            let end = json_object_end(&buf, start);
            if end == -1 {
                // Object still arriving — resume from the marker on the next chunk.
                long.window = buf[idx..].to_string();
                return;
            }
            if end == -2 {
                base = start;
                continue;
            }
            let end = end as usize;
            if let Ok(obj) = serde_json::from_str::<Value>(&buf[start..end]) {
                // A tool schema may have a property named `usage` — only an object carrying a
                // numeric token count is the real one.
                if obj.is_object()
                    && (num(obj.get("input_tokens")).is_some() || num(obj.get("prompt_tokens")).is_some())
                {
                    long.found = Some(obj);
                }
            }
            base = end;
        }
    }

    fn end_long(&mut self) {
        let Some(long) = self.long_line.take() else { return };
        self.event_name = String::new();
        if !long.terminal {
            return;
        }
        self.seen_completion = true;
        if let Some(found) = long.found {
            self.usage = Some(found);
        }
    }
}

impl UsageSniffer for OpenAiSseUsageSniffer {
    fn feed(&mut self, chunk: &[u8]) {
        let decoded = self.decoder.decode(chunk);
        let mut pos = 0usize;
        while pos < decoded.len() {
            if self.long_line.is_some() {
                match decoded[pos..].find('\n') {
                    None => {
                        let text = decoded[pos..].to_string();
                        self.scan_long(&text);
                        return;
                    }
                    Some(nl) => {
                        let text = decoded[pos..pos + nl].to_string();
                        self.scan_long(&text);
                        self.end_long();
                        pos += nl + 1;
                        continue;
                    }
                }
            }
            let rest = &decoded[pos..];
            if !rest.contains('\n') {
                self.carry.push_str(rest);
            } else {
                let joined = format!("{}{}", self.carry, rest);
                let mut parts: Vec<&str> = joined.split('\n').collect();
                let carry = parts.pop().unwrap_or("").to_string();
                let lines: Vec<String> = parts.into_iter().map(str::to_string).collect();
                self.carry = carry;
                for line in lines {
                    self.process_line(&line);
                }
            }
            pos = decoded.len();
            if self.carry.len() > MAX_CARRY {
                let carry = std::mem::take(&mut self.carry);
                let terminal = self.is_terminal_line(&carry);
                self.long_line = Some(LongLine { terminal, window: String::new(), found: None });
                self.scan_long(&carry);
            }
        }
    }

    fn finish(&self) -> Option<NormalizedUsage> {
        self.usage.as_ref().map(|u| from_openai_usage(Some(u)))
    }

    fn complete(&self) -> bool {
        self.seen_completion
    }
}

/// Anthropic Messages SSE. `message_start` seeds the input-side counts (+ cache fields),
/// `message_delta` carries the output-side count — and, on newer API revisions, may repeat
/// cumulative input/cache fields too. Merged field-wise: the last non-absent value seen for
/// each field wins.
#[derive(Default)]
pub struct AnthropicSseUsageSniffer {
    decoder: Utf8Decoder,
    carry: String,
    /// An over-long line is being skipped up to its newline.
    skipping: bool,
    event: String,
    seen: bool,
    seen_message_stop: bool,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
}

pub fn create_anthropic_sse_usage_sniffer() -> Box<dyn UsageSniffer> {
    Box::new(AnthropicSseUsageSniffer::default())
}

impl AnthropicSseUsageSniffer {
    fn merge(&mut self, partial: Option<&Value>) {
        let Some(partial) = partial.filter(|v| v.is_object()) else { return };
        self.seen = true;
        if let Some(v) = num(partial.get("input_tokens")) {
            self.input_tokens = Some(v);
        }
        if let Some(v) = num(partial.get("output_tokens")) {
            self.output_tokens = Some(v);
        }
        if let Some(v) = num(partial.get("cache_read_input_tokens")) {
            self.cache_read_input_tokens = Some(v);
        }
        if let Some(v) = num(partial.get("cache_creation_input_tokens")) {
            self.cache_creation_input_tokens = Some(v);
        }
    }

    fn process_line(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("event:") {
            self.event = rest.trim().to_string();
            return;
        }
        let Some(rest) = line.strip_prefix("data:") else { return };
        let data = rest.trim().to_string();
        let current_event = std::mem::take(&mut self.event);
        if data.is_empty() {
            return;
        }
        // message_stop's payload (`{"type":"message_stop"}`) never carries a "usage" substring,
        // so this check must happen before that fast filter.
        if current_event == "message_stop" {
            self.seen_message_stop = true;
            return;
        }
        if !data.contains("\"usage\"") {
            return;
        }
        let Ok(json) = serde_json::from_str::<Value>(&data) else { return };
        if current_event == "message_start" {
            let usage = json.get("message").and_then(|m| m.get("usage")).cloned();
            self.merge(usage.as_ref());
        } else if current_event == "message_delta" {
            let usage = json.get("usage").cloned();
            self.merge(usage.as_ref());
        }
    }
}

impl UsageSniffer for AnthropicSseUsageSniffer {
    fn feed(&mut self, chunk: &[u8]) {
        let decoded = self.decoder.decode(chunk);
        let mut pos = 0usize;
        while pos < decoded.len() {
            if self.skipping {
                match decoded[pos..].find('\n') {
                    None => return,
                    Some(nl) => {
                        self.skipping = false;
                        self.event = String::new();
                        pos += nl + 1;
                        continue;
                    }
                }
            }
            self.carry.push_str(&decoded[pos..]);
            pos = decoded.len();
            let joined = std::mem::take(&mut self.carry);
            let mut parts: Vec<&str> = joined.split('\n').collect();
            let carry = parts.pop().unwrap_or("").to_string();
            let lines: Vec<String> = parts.into_iter().map(str::to_string).collect();
            self.carry = carry;
            for line in lines {
                self.process_line(&line);
            }
            if self.carry.len() > MAX_CARRY {
                self.carry = String::new();
                self.skipping = true;
            }
        }
    }

    fn finish(&self) -> Option<NormalizedUsage> {
        if !self.seen {
            return None;
        }
        let mut usage = Map::new();
        for (key, value) in [
            ("input_tokens", self.input_tokens),
            ("output_tokens", self.output_tokens),
            ("cache_read_input_tokens", self.cache_read_input_tokens),
            ("cache_creation_input_tokens", self.cache_creation_input_tokens),
        ] {
            if let Some(value) = value {
                usage.insert(key.to_string(), Value::from(value));
            }
        }
        Some(from_anthropic_usage(Some(&Value::Object(usage))))
    }

    fn complete(&self) -> bool {
        self.seen_message_stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(prompt: Option<i64>, completion: Option<i64>, read: Option<i64>, creation: Option<i64>) -> NormalizedUsage {
        NormalizedUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            cache_read_input_tokens: read,
            cache_creation_input_tokens: creation,
        }
    }

    fn byte_chunks(text: &str, size: usize) -> Vec<Vec<u8>> {
        text.as_bytes().chunks(size).map(<[u8]>::to_vec).collect()
    }

    fn d(value: Value) -> String {
        format!("data: {value}")
    }

    // ---- fromOpenAIUsage ----

    #[test]
    fn keeps_completion_tokens_without_double_adding_reasoning_tokens() {
        let out = from_openai_usage(Some(&json!({
            "prompt_tokens": 100, "completion_tokens": 40,
            "completion_tokens_details": { "reasoning_tokens": 15 }
        })));
        assert_eq!(out.prompt_tokens, Some(100));
        assert_eq!(out.completion_tokens, Some(40));
    }

    #[test]
    fn leaves_completion_tokens_unchanged_when_reasoning_tokens_is_absent() {
        assert_eq!(
            from_openai_usage(Some(&json!({"prompt_tokens": 10, "completion_tokens": 4}))).completion_tokens,
            Some(4)
        );
    }

    #[test]
    fn does_not_fabricate_completion_tokens_from_reasoning_tokens_alone() {
        let out = from_openai_usage(Some(&json!({
            "prompt_tokens": 10, "completion_tokens_details": { "reasoning_tokens": 15 }
        })));
        assert_eq!(out.completion_tokens, None);
    }

    #[test]
    fn stores_prompt_tokens_as_is_and_completion_tokens() {
        assert_eq!(
            from_openai_usage(Some(&json!({"prompt_tokens": 100, "completion_tokens": 40}))),
            usage(Some(100), Some(40), None, None)
        );
    }

    #[test]
    fn absent_openai_cache_details_are_null_never_zero() {
        let out = from_openai_usage(Some(&json!({"prompt_tokens": 5, "completion_tokens": 1})));
        assert_eq!(out.cache_read_input_tokens, None);
        assert_eq!(out.cache_creation_input_tokens, None);
    }

    #[test]
    fn reads_cache_fields_from_prompt_tokens_details() {
        assert_eq!(
            from_openai_usage(Some(&json!({
                "prompt_tokens": 100, "completion_tokens": 40,
                "prompt_tokens_details": { "cached_tokens": 20, "cache_write_tokens": 6 }
            }))),
            usage(Some(100), Some(40), Some(20), Some(6))
        );
    }

    #[test]
    fn uses_the_proxy_cache_creation_extension_only_without_an_upstream_cache_write() {
        assert_eq!(
            from_openai_usage(Some(&json!({
                "prompt_tokens": 100, "completion_tokens": 40,
                "prompt_tokens_details": { "cache_write_tokens": 6 },
                "cache_creation_input_tokens": 9
            })))
            .cache_creation_input_tokens,
            Some(6)
        );
        assert_eq!(
            from_openai_usage(Some(&json!({
                "prompt_tokens": 100, "completion_tokens": 40, "cache_creation_input_tokens": 9
            })))
            .cache_creation_input_tokens,
            Some(9)
        );
    }

    #[test]
    fn cached_tokens_zero_is_a_real_reported_value() {
        let out = from_openai_usage(Some(&json!({
            "prompt_tokens": 10, "completion_tokens": 2, "prompt_tokens_details": { "cached_tokens": 0 }
        })));
        assert_eq!(out.cache_read_input_tokens, Some(0));
    }

    #[test]
    fn returns_all_null_for_a_missing_openai_usage_object() {
        assert_eq!(from_openai_usage(None), NULL_USAGE);
        assert_eq!(from_openai_usage(Some(&Value::Null)), NULL_USAGE);
    }

    #[test]
    fn reads_the_responses_shape_input_output_and_input_tokens_details() {
        assert_eq!(
            from_openai_usage(Some(&json!({
                "input_tokens": 82665, "output_tokens": 14,
                "input_tokens_details": { "cached_tokens": 82432 }
            }))),
            usage(Some(82665), Some(14), Some(82432), None)
        );
    }

    // ---- fromAnthropicUsage ----

    #[test]
    fn sums_input_and_cache_fields_into_the_prompt_total() {
        assert_eq!(
            from_anthropic_usage(Some(&json!({
                "input_tokens": 10, "output_tokens": 5,
                "cache_read_input_tokens": 2, "cache_creation_input_tokens": 3
            }))),
            usage(Some(15), Some(5), Some(2), Some(3))
        );
    }

    #[test]
    fn absent_anthropic_cache_fields_default_to_zero() {
        assert_eq!(
            from_anthropic_usage(Some(&json!({"input_tokens": 10, "output_tokens": 5}))),
            usage(Some(10), Some(5), Some(0), Some(0))
        );
    }

    #[test]
    fn a_wholly_absent_anthropic_usage_object_is_null() {
        assert_eq!(from_anthropic_usage(None), NULL_USAGE);
        assert_eq!(from_anthropic_usage(Some(&Value::Null)), NULL_USAGE);
    }

    #[test]
    fn missing_output_tokens_is_null_not_zero() {
        assert_eq!(from_anthropic_usage(Some(&json!({"input_tokens": 10}))).completion_tokens, None);
    }

    // ---- createOpenAISseUsageSniffer ----

    #[test]
    fn captures_usage_from_the_final_chunk_split_across_arbitrary_boundaries() {
        let sse = [
            d(json!({"choices":[{"index":0,"delta":{"content":"hi"}}]})),
            String::new(),
            d(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})),
            String::new(),
            d(json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50}})),
            String::new(),
            "data: [DONE]".to_string(),
            String::new(),
        ]
        .join("\n");
        let mut sniffer = OpenAiSseUsageSniffer::default();
        for chunk in byte_chunks(&sse, 7) {
            sniffer.feed(&chunk);
        }
        assert_eq!(sniffer.finish(), Some(usage(Some(100), Some(50), None, None)));
    }

    #[test]
    fn captures_openai_cache_fields() {
        let sse = d(json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 100, "completion_tokens": 50,
                "prompt_tokens_details": { "cached_tokens": 20 },
                "cache_creation_input_tokens": 5
            }
        })) + "\n";
        let mut sniffer = OpenAiSseUsageSniffer::default();
        for chunk in byte_chunks(&sse, 11) {
            sniffer.feed(&chunk);
        }
        assert_eq!(sniffer.finish(), Some(usage(Some(100), Some(50), Some(20), Some(5))));
    }

    #[test]
    fn reassembles_a_usage_line_split_mid_line_reporting_nothing_until_the_newline() {
        let line = d(json!({"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":7}}));
        let mut sniffer = OpenAiSseUsageSniffer::default();
        let bytes = line.as_bytes();
        let mid = bytes.len() / 2;
        sniffer.feed(&bytes[..mid]);
        sniffer.feed(&bytes[mid..]);
        assert_eq!(sniffer.finish(), None);
        sniffer.feed(b"\n");
        assert_eq!(sniffer.finish(), Some(usage(Some(42), Some(7), None, None)));
    }

    #[test]
    fn returns_none_when_no_chunk_ever_carried_usage() {
        let sse = [
            d(json!({"choices":[{"index":0,"delta":{"content":"hi"}}]})),
            String::new(),
            "data: [DONE]".to_string(),
            String::new(),
        ]
        .join("\n");
        let mut sniffer = OpenAiSseUsageSniffer::default();
        for chunk in byte_chunks(&sse, 9) {
            sniffer.feed(&chunk);
        }
        assert_eq!(sniffer.finish(), None);
    }

    #[test]
    fn tolerates_a_malformed_data_line_and_still_captures_a_later_good_one() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        sniffer.feed(b"data: {\"usage\": not valid json\n");
        let good = d(json!({"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":1}})) + "\n";
        sniffer.feed(good.as_bytes());
        assert_eq!(sniffer.finish(), Some(usage(Some(3), Some(1), None, None)));
    }

    #[test]
    fn skips_an_over_long_non_terminal_line_and_keeps_capturing() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        let huge = format!("data: {}", "x".repeat(300 * 1024));
        sniffer.feed(huge.as_bytes());
        assert_eq!(sniffer.finish(), None);
        let next = format!("\n{}\n", d(json!({"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":9}})));
        sniffer.feed(next.as_bytes());
        let out = sniffer.finish().unwrap();
        assert_eq!((out.prompt_tokens, out.completion_tokens), (Some(9), Some(9)));
    }

    // ---- over-long Responses terminal event ----

    fn responses_usage() -> Value {
        json!({
            "input_tokens": 82665,
            "input_tokens_details": { "cached_tokens": 82432 },
            "output_tokens": 14,
            "output_tokens_details": { "reasoning_tokens": 0 },
            "total_tokens": 82679
        })
    }

    /// A `response.completed` event the size of a real Codex turn: the request is echoed back
    /// ahead of `usage`.
    fn terminal(event_type: &str, filler: usize, extra_usage: Option<Value>) -> String {
        let mut response = serde_json::Map::new();
        response.insert("id".into(), json!("resp_1"));
        response.insert("object".into(), json!("response"));
        response.insert(
            "status".into(),
            json!(if event_type == "response.completed" { "completed" } else { "incomplete" }),
        );
        response.insert("instructions".into(), json!("i".repeat(filler)));
        response.insert(
            "tools".into(),
            json!([{ "type": "function", "name": "shell", "parameters": { "properties": { "usage": { "type": "string" } } } }]),
        );
        response.insert(
            "output".into(),
            json!([{ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "hi" }] }]),
        );
        if let Some(extra) = extra_usage {
            response.insert("usage".into(), extra);
        } else {
            response.insert("usage".into(), responses_usage());
        }
        response.insert("user".into(), Value::Null);
        response.insert("metadata".into(), json!({}));
        let payload = json!({ "type": event_type, "sequence_number": 42, "response": Value::Object(response) });
        format!("event: {event_type}\n{}\n\n", d(payload))
    }

    #[test]
    fn captures_usage_from_a_response_completed_line_far_past_the_carry_cap() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        for chunk in byte_chunks(&terminal("response.completed", 600 * 1024, None), 16 * 1024) {
            sniffer.feed(&chunk);
        }
        assert_eq!(sniffer.finish(), Some(usage(Some(82665), Some(14), Some(82432), None)));
        assert!(sniffer.complete());
    }

    #[test]
    fn works_when_the_usage_marker_and_object_straddle_chunk_boundaries() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        for chunk in byte_chunks(&terminal("response.incomplete", 300 * 1024, None), 7) {
            sniffer.feed(&chunk);
        }
        let out = sniffer.finish().unwrap();
        assert_eq!(
            (out.prompt_tokens, out.completion_tokens, out.cache_read_input_tokens),
            (Some(82665), Some(14), Some(82432))
        );
        assert!(sniffer.complete());
    }

    #[test]
    fn identifies_the_terminal_event_by_its_leading_type_without_an_event_line() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        let full = terminal("response.completed", 300 * 1024, None);
        let line = full.split_once('\n').expect("event line").1.to_string();
        for chunk in byte_chunks(&line, 32 * 1024) {
            sniffer.feed(&chunk);
        }
        assert_eq!(sniffer.finish().unwrap().prompt_tokens, Some(82665));
        assert!(sniffer.complete());
    }

    #[test]
    fn identifies_it_by_the_event_line_when_the_type_key_is_not_first() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        let line = format!(
            "event: response.completed\n{}\n\n",
            d(json!({
                "sequence_number": 1,
                "response": { "instructions": "i".repeat(300 * 1024), "usage": responses_usage() },
                "type": "response.completed"
            }))
        );
        for chunk in byte_chunks(&line, 32 * 1024) {
            sniffer.feed(&chunk);
        }
        let out = sniffer.finish().unwrap();
        assert_eq!((out.prompt_tokens, out.cache_read_input_tokens), (Some(82665), Some(82432)));
        assert!(sniffer.complete());
    }

    #[test]
    fn ignores_a_tool_schemas_usage_property_and_a_null_usage() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        // An over-long response.created with "usage":null is skipped as a non-terminal line …
        for chunk in byte_chunks(&terminal("response.created", 300 * 1024, Some(Value::Null)), 32 * 1024) {
            sniffer.feed(&chunk);
        }
        assert_eq!(sniffer.finish(), None);
        assert!(!sniffer.complete());
        // … then the terminal line, whose tool schema mentions `usage` before the real object.
        for chunk in byte_chunks(&terminal("response.completed", 300 * 1024, None), 32 * 1024) {
            sniffer.feed(&chunk);
        }
        let out = sniffer.finish().unwrap();
        assert_eq!((out.prompt_tokens, out.completion_tokens), (Some(82665), Some(14)));
        assert!(sniffer.complete());
    }

    #[test]
    fn a_short_response_completed_goes_through_the_ordinary_line_parser() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        sniffer.feed(terminal("response.completed", 10, None).as_bytes());
        let out = sniffer.finish().unwrap();
        assert_eq!((out.prompt_tokens, out.cache_read_input_tokens), (Some(82665), Some(82432)));
        assert!(sniffer.complete());
    }

    // ---- createAnthropicSseUsageSniffer ----

    #[test]
    fn merges_message_start_input_with_message_delta_output_field_wise() {
        let sse = [
            "event: message_start".to_string(),
            d(json!({
                "type": "message_start",
                "message": { "id": "msg_1", "usage": { "input_tokens": 10, "cache_read_input_tokens": 2, "cache_creation_input_tokens": 3 } }
            })),
            String::new(),
            "event: content_block_delta".to_string(),
            d(json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}})),
            String::new(),
            "event: message_delta".to_string(),
            d(json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}})),
            String::new(),
            "event: message_stop".to_string(),
            d(json!({"type":"message_stop"})),
            String::new(),
        ]
        .join("\n");
        let mut sniffer = AnthropicSseUsageSniffer::default();
        for chunk in byte_chunks(&sse, 17) {
            sniffer.feed(&chunk);
        }
        assert_eq!(sniffer.finish(), Some(usage(Some(15), Some(7), Some(2), Some(3))));
    }

    #[test]
    fn reassembles_event_and_data_lines_split_across_arbitrary_boundaries() {
        let sse = [
            "event: message_start".to_string(),
            d(json!({"type":"message_start","message":{"usage":{"input_tokens":5}}})),
            String::new(),
        ]
        .join("\n");
        let mut sniffer = AnthropicSseUsageSniffer::default();
        for chunk in byte_chunks(&sse, 3) {
            sniffer.feed(&chunk);
        }
        assert_eq!(sniffer.finish(), Some(usage(Some(5), None, Some(0), Some(0))));
    }

    #[test]
    fn returns_none_when_neither_anthropic_event_carried_usage() {
        let sse = [
            "event: message_start".to_string(),
            d(json!({"type":"message_start","message":{"id":"msg_1"}})),
            String::new(),
            "event: message_stop".to_string(),
            d(json!({"type":"message_stop"})),
            String::new(),
        ]
        .join("\n");
        let mut sniffer = AnthropicSseUsageSniffer::default();
        for chunk in byte_chunks(&sse, 13) {
            sniffer.feed(&chunk);
        }
        assert_eq!(sniffer.finish(), None);
    }

    #[test]
    fn a_cumulative_repeat_on_message_delta_wins_field_wise() {
        let sse = [
            "event: message_start".to_string(),
            d(json!({"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":2}}})),
            String::new(),
            "event: message_delta".to_string(),
            d(json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 7, "input_tokens": 12, "cache_read_input_tokens": 4 }
            })),
            String::new(),
        ]
        .join("\n");
        let mut sniffer = AnthropicSseUsageSniffer::default();
        for chunk in byte_chunks(&sse, 23) {
            sniffer.feed(&chunk);
        }
        // input 12 (last-wins) + cache_read 4 (last-wins) + cache_creation 0 (never sent).
        assert_eq!(sniffer.finish(), Some(usage(Some(16), Some(7), Some(4), Some(0))));
    }

    #[test]
    fn anthropic_tolerates_a_malformed_data_line() {
        let mut sniffer = AnthropicSseUsageSniffer::default();
        sniffer.feed(b"event: message_start\ndata: {\"usage\": broken\n");
        let next = format!("event: message_delta\n{}\n", d(json!({"type":"message_delta","usage":{"output_tokens":4}})));
        sniffer.feed(next.as_bytes());
        assert_eq!(sniffer.finish(), Some(usage(Some(0), Some(4), Some(0), Some(0))));
    }

    #[test]
    fn anthropic_skips_an_over_long_line_and_keeps_listening() {
        let mut sniffer = AnthropicSseUsageSniffer::default();
        let huge = format!("event: content_block_delta\ndata: {}", "y".repeat(300 * 1024));
        sniffer.feed(huge.as_bytes());
        assert_eq!(sniffer.finish(), None);
        let next = format!(
            "\nevent: message_delta\ndata: {}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
            json!({"usage":{"output_tokens":4}})
        );
        sniffer.feed(next.as_bytes());
        assert_eq!(sniffer.finish().unwrap().completion_tokens, Some(4));
        assert!(sniffer.complete());
    }

    // ---- complete() ----

    #[test]
    fn openai_complete_is_false_before_anything_is_fed() {
        assert!(!OpenAiSseUsageSniffer::default().complete());
    }

    #[test]
    fn done_marks_the_stream_complete() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        sniffer.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n");
        assert!(sniffer.complete());
    }

    #[test]
    fn a_real_finish_reason_marks_it_complete_without_a_literal_done() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        let line = d(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})) + "\n\n";
        sniffer.feed(line.as_bytes());
        assert!(sniffer.complete());
    }

    #[test]
    fn finish_reason_null_does_not_mark_it_complete() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        let line = d(json!({"choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]})) + "\n\n";
        sniffer.feed(line.as_bytes());
        assert!(!sniffer.complete());
    }

    #[test]
    fn no_completion_signal_leaves_it_incomplete() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        let line = d(json!({"choices":[{"delta":{"content":"hi"}}]})) + "\n\n";
        sniffer.feed(line.as_bytes());
        assert!(!sniffer.complete());
    }

    #[test]
    fn an_over_long_non_terminal_line_neither_completes_nor_undoes_a_completion() {
        let mut sniffer = OpenAiSseUsageSniffer::default();
        let huge = format!("data: {}", "x".repeat(300 * 1024));
        sniffer.feed(format!("{huge}\n").as_bytes());
        assert!(!sniffer.complete());
        sniffer.feed(b"data: [DONE]\n\n");
        assert!(sniffer.complete());
        sniffer.feed(huge.as_bytes());
        assert!(sniffer.complete());
    }

    #[test]
    fn anthropic_complete_is_false_before_anything_is_fed() {
        assert!(!AnthropicSseUsageSniffer::default().complete());
    }

    #[test]
    fn message_stop_marks_the_stream_complete_even_with_no_usage() {
        let mut sniffer = AnthropicSseUsageSniffer::default();
        let line = format!("event: message_stop\n{}\n", d(json!({"type":"message_stop"})));
        sniffer.feed(line.as_bytes());
        assert!(sniffer.complete());
        assert_eq!(sniffer.finish(), None);
    }

    #[test]
    fn no_message_stop_leaves_it_incomplete_even_with_usage() {
        let mut sniffer = AnthropicSseUsageSniffer::default();
        let line = format!(
            "event: message_start\n{}\n",
            d(json!({"type":"message_start","message":{"usage":{"input_tokens":5}}}))
        );
        sniffer.feed(line.as_bytes());
        assert!(!sniffer.complete());
    }

    #[test]
    fn an_over_long_anthropic_line_neither_completes_nor_undoes_a_message_stop() {
        let mut sniffer = AnthropicSseUsageSniffer::default();
        let huge = format!("data: {}", "z".repeat(300 * 1024));
        sniffer.feed(format!("{huge}\n").as_bytes());
        assert!(!sniffer.complete());
        sniffer.feed(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        assert!(sniffer.complete());
        sniffer.feed(huge.as_bytes());
        assert!(sniffer.complete());
    }

    #[test]
    fn streaming_utf8_decode_joins_a_sequence_split_across_chunks() {
        let mut decoder = Utf8Decoder::default();
        let bytes = "台灣🙂".as_bytes();
        let mut out = String::new();
        for byte in bytes {
            out.push_str(&decoder.decode(&[*byte]));
        }
        assert_eq!(out, "台灣🙂");
    }
}
