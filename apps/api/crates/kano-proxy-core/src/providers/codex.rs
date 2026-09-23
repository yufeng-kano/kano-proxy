//! The Codex provider (docs/providers.md § Codex, docs/api.md
//! § `POST /openai/v1/responses`).
//!
//! ChatGPT `codex/responses` adapter: Chat Completions ↔ Responses SSE on the conversion
//! path, and the client's own Responses body forwarded after the documented fix-ups on the
//! native path. Egress is direct to chatgpt.com through `AppState::transport()` — the Cloud
//! Run relay exists only because Cloudflare stamps `CF-Worker` on every Worker subrequest,
//! and this server is not a Worker (docs/rust-server.md § Why). The client-visible
//! behaviors the relay used to guarantee are preserved without it: an upstream `413` is
//! surfaced as-is with its own body, a failed token count degrades to `{"input_tokens": 0}`
//! (providers/codex_count.rs), and nothing synthesizes an auth-shaped status that would
//! bench an account for a fault that is not its own.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::response::Response;
use base64::Engine;
use bytes::Bytes;
use http::StatusCode;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::db::accounts::{iso_from_ms, parse_iso_ms};
use crate::pool::{AcquiredAccount, StoredCredential};
use crate::proxy::codex_openai::{codex_sse_to_openai_stream, collect_codex_sse, CodexCollected, CodexSseOptions};
use crate::proxy::responses_openai::{collect_responses_sse, CollectedResponses};
use crate::upstream::{UpstreamRequest, UpstreamResponse};
use crate::utils::reasoning::map_reasoning;
use crate::AppState;

use super::codex_models::fetch_codex_models;
use super::codex_reasoning_cache::{
    append_codex_replay_turn, codex_reasoning_replay_session_key, read_codex_reasoning_replay,
    write_codex_reasoning_replay, CodexReasoningReplayEntry,
};
use super::codex_replay_history::{codex_input_hashes, codex_replay_turn, replay_codex_history};
use super::codex_usage::{fetch_codex_usage_json, windows_from_codex_payload};
use super::refresh::refresh_oauth_credential;
use super::types::{
    AdapterError, AffinityIds, CallExtras, ChatCompletionRequest, DynAdapter, FetchedUsage, ListedModels,
    ProviderAdapter,
};
use super::ProviderId;

const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// The Codex TUI identity every `/codex/responses` call advertises; the version tracks
/// [`CODEX_CLIENT_VERSION`] (asserted in this module's tests).
pub const CODEX_USER_AGENT: &str =
    "codex-tui/0.156.0 (Mac OS 26.5.0; arm64) iTerm.app/3.6.10 (codex-tui; 0.156.0)";
pub const CODEX_ORIGINATOR: &str = "codex-tui";

/// OpenAI's documented `prompt_cache_key` ceiling; over it is a hard 400.
const CODEX_PROMPT_CACHE_KEY_MAX: usize = 64;

static SESSION_UUID_SUFFIX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"_session_([0-9a-fA-F-]{36})$").expect("static regex compiles"));

// ---------------------------------------------------------------------------
// Credential refresh
// ---------------------------------------------------------------------------

fn client_id(cx: &AppState) -> String {
    cx.config()
        .codex_oauth_client_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string())
}

fn codex_needs_refresh(credential: &StoredCredential) -> bool {
    if credential.refresh_token.as_deref().unwrap_or("").is_empty() {
        return false;
    }
    let exp = credential.expires_at.as_deref().and_then(parse_iso_ms).unwrap_or(0);
    exp == 0 || exp - 60_000 <= crate::app::now_ms()
}

async fn refresh_codex_credential(
    cx: &AppState,
    credential: StoredCredential,
    fallback_client_id: String,
) -> Option<StoredCredential> {
    let refresh_token = credential.refresh_token.clone()?;
    let client_id =
        credential.client_id.clone().filter(|s| !s.is_empty()).unwrap_or(fallback_client_id);
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", &refresh_token)
        .append_pair("client_id", &client_id)
        .finish();
    let request = UpstreamRequest::post(TOKEN_URL)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(form));
    let response = cx.transport().send(request).await.ok()?;
    if !response.status.is_success() {
        return None;
    }
    let json = response.json_value().await.ok()?;
    // An answer without an access token is not a refresh; keep the stored credential rather
    // than persisting a blank one.
    let access_token = json.get("access_token")?.as_str()?.to_string();
    let mut next = credential;
    next.access_token = access_token;
    if let Some(token) = json.get("refresh_token").and_then(Value::as_str) {
        next.refresh_token = Some(token.to_string());
    }
    if let Some(expires_in) = json.get("expires_in").and_then(Value::as_i64).filter(|v| *v != 0) {
        next.expires_at = Some(iso_from_ms(crate::app::now_ms() + expires_in * 1000));
    }
    Some(next)
}

async fn refresh_codex(cx: &AppState, account: AcquiredAccount) -> AcquiredAccount {
    let fallback = client_id(cx);
    refresh_oauth_credential(cx, account, codex_needs_refresh, |credential| {
        let cx = cx.clone();
        let credential = credential.clone();
        let fallback = fallback.clone();
        async move { refresh_codex_credential(&cx, credential, fallback).await }
    })
    .await
}

/// `chatgpt_account_id` from the ChatGPT OAuth access token's own claims.
fn account_id_from_jwt(access: &str) -> Option<String> {
    let payload = access.split('.').nth(1).filter(|p| !p.is_empty())?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let json: Value = serde_json::from_slice(&bytes).ok()?;
    json.get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn chatgpt_account_id(credential: &StoredCredential) -> String {
    credential
        .account_id
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| account_id_from_jwt(&credential.access_token))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Session / prompt_cache_key identifiers
// ---------------------------------------------------------------------------

/// Candidate `session_id` header value from the request's `prompt_cache_key` (client-sent on
/// `/openai/v1`, `metadata.user_id`-derived on `/anthropic`). A key ending in
/// `_session_<uuid>` contributes that bare UUID, matching what the Codex CLI itself puts in
/// this header; any other non-empty key is used as-is. The caller still fits the winner.
pub fn codex_session_id(prompt_cache_key: Option<&str>) -> Option<String> {
    let key = prompt_cache_key.map(str::trim).filter(|s| !s.is_empty())?;
    Some(session_uuid(key).unwrap_or_else(|| key.to_string()))
}

fn session_uuid(key: &str) -> Option<String> {
    SESSION_UUID_SUFFIX.captures(key).map(|c| c[1].to_string())
}

/// Fit a cache/session identifier into the Responses `prompt_cache_key` 64-char limit.
/// Applies to BOTH the body field and the `session_id` header: the backend validates the
/// header under the `prompt_cache_key` name, so an over-long header 400s the turn even when
/// the body field is already fitted. Deterministic — a conversation only gets upstream cache
/// hits if its value is stable. Long keys are hashed rather than truncated: client ids share
/// long fixed prefixes, so a prefix cut would collapse every session of one account onto a
/// single cache shard.
pub fn fit_codex_prompt_cache_key(key: &str) -> String {
    if let Some(uuid) = session_uuid(key) {
        return uuid;
    }
    if key.chars().count() <= CODEX_PROMPT_CACHE_KEY_MAX {
        return key.to_string();
    }
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// A `crypto.randomUUID()` equivalent: a v4 UUID used only when no client identifier exists.
fn random_uuid() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// The `session_id` header value actually sent upstream: client affinity ids first, then the
/// prompt_cache_key-derived candidate, then a per-request random UUID — each fitted to the
/// 64-char ceiling above, because affinity ids are opaque client input and are just as
/// capable of exceeding the limit.
pub fn codex_session_id_header(affinity: Option<&AffinityIds>, prompt_cache_key: Option<&str>) -> String {
    let from_affinity = affinity.and_then(|a| {
        a.session_id
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| a.conv_id.clone().filter(|s| !s.is_empty()))
    });
    match from_affinity.or_else(|| codex_session_id(prompt_cache_key)) {
        Some(candidate) => fit_codex_prompt_cache_key(&candidate),
        None => random_uuid(),
    }
}

// ---------------------------------------------------------------------------
// Request body builders (pure — no network, unit-testable directly)
// ---------------------------------------------------------------------------

/// Responses rejects a `call_id` over 64 chars, and Claude Code emits tool ids long enough to
/// hit that. Shorten to a 64-char prefix plus a hash suffix, matching the reference proxy so
/// a `function_call` and its `function_call_output` still resolve to the same id.
pub fn shorten_codex_call_id(id: &Value) -> Value {
    let Some(text) = id.as_str() else { return id.clone() };
    if text.chars().count() <= 64 {
        return id.clone();
    }
    let hex = hex::encode(Sha256::digest(text.as_bytes()));
    let suffix = format!("_{}", &hex[..16]);
    let keep = 64 - suffix.len();
    let head: String = text.chars().take(keep).collect();
    Value::String(format!("{head}{suffix}"))
}

const CODEX_REJECTED_BODY_FIELDS: [&str; 11] = [
    "max_output_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "truncation",
    "user",
    "previous_response_id",
    "generate",
    "prompt_cache_retention",
    "safety_identifier",
    "stream_options",
];

/// Remove fields rejected by `/codex/responses`, without touching valid fields.
fn strip_rejected_codex_fields(mut body: Map<String, Value>) -> Map<String, Value> {
    for field in CODEX_REJECTED_BODY_FIELDS {
        body.remove(field);
    }
    if body.get("service_tier").and_then(Value::as_str) != Some("priority") {
        body.remove("service_tier");
    }
    body
}

/// OpenAI Chat Completions `tool_choice` → Responses flattened shape. Upstream may reject
/// `tool_choice` sent without `tools`, so callers gate `has_tools` themselves.
fn map_codex_tool_choice(tool_choice: Option<&Value>, has_tools: bool) -> Option<Value> {
    if !has_tools {
        return None;
    }
    let Some(choice) = tool_choice.filter(|v| !v.is_null()) else {
        return Some(Value::String("auto".into()));
    };
    if matches!(choice.as_str(), Some("auto" | "none" | "required")) {
        return Some(choice.clone());
    }
    if let Some(obj) = choice.as_object() {
        if obj.get("type").and_then(Value::as_str) == Some("function") {
            if let Some(name) = obj.get("function").and_then(|f| f.get("name")).and_then(Value::as_str) {
                return Some(json!({ "type": "function", "name": name }));
            }
        }
    }
    Some(choice.clone())
}

/// JavaScript `String(value)` for the values a content part can carry.
fn js_string(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Array(_)) => value.map(|v| v.to_string()).unwrap_or_default(),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|p| match p {
                Value::Object(o) if o.get("type").and_then(Value::as_str) == Some("text") => {
                    js_string(o.get("text"))
                }
                Value::String(s) => s.clone(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn content_to_codex(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(s)) => vec![json!({ "type": "input_text", "text": s })],
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for part in parts {
                let Some(p) = part.as_object() else { continue };
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let mut item = Map::new();
                        item.insert("type".into(), Value::String("input_text".into()));
                        if let Some(text) = p.get("text") {
                            item.insert("text".into(), text.clone());
                        }
                        out.push(Value::Object(item));
                    }
                    Some("image_url") => {
                        let image = p.get("image_url");
                        let url = image.and_then(|i| i.get("url")).and_then(Value::as_str);
                        if let Some(url) = url.filter(|u| !u.is_empty()) {
                            let mut item = Map::new();
                            item.insert("type".into(), Value::String("input_image".into()));
                            item.insert("image_url".into(), Value::String(url.to_string()));
                            if let Some(detail) = image.and_then(|i| i.get("detail")).and_then(Value::as_str) {
                                item.insert("detail".into(), Value::String(detail.to_string()));
                            }
                            out.push(Value::Object(item));
                        }
                    }
                    _ => {}
                }
            }
            if out.is_empty() {
                out.push(json!({ "type": "input_text", "text": "" }));
            }
            out
        }
        other => vec![json!({ "type": "input_text", "text": js_string(other) })],
    }
}

fn map_tools(tools: Option<&Value>) -> Vec<Value> {
    let Some(Value::Array(tools)) = tools else { return Vec::new() };
    tools
        .iter()
        .map(|t| {
            let Some(function) = t.get("function").filter(|f| f.is_object()) else { return t.clone() };
            let mut out = Map::new();
            out.insert("type".into(), Value::String("function".into()));
            for key in ["name", "description", "parameters"] {
                if let Some(v) = function.get(key) {
                    out.insert(key.into(), v.clone());
                }
            }
            Value::Object(out)
        })
        .collect()
}

/// `role: "system"` messages are pulled out of the message list entirely — they become the
/// top-level Responses `instructions` field, not fake `role: "user"` input items.
fn openai_messages_to_codex_input(messages: &[Value]) -> Vec<Value> {
    let mut input: Vec<Value> = Vec::new();
    for m in messages {
        let role = m.get("role").map(|r| js_string(Some(r))).unwrap_or_default();
        match role.as_str() {
            // The normal builder hoists system messages into top-level instructions before
            // calling this mapper. Keep this defensive path Responses-valid if a system item
            // reaches the input mapper from another caller.
            "system" => input.push(json!({ "role": "developer", "content": content_to_codex(m.get("content")) })),
            "user" => input.push(json!({ "role": "user", "content": content_to_codex(m.get("content")) })),
            "assistant" => {
                // Text precedes the tool calls it led up to — matching the order the original
                // message carried them in.
                let text = content_text(m.get("content"));
                if !text.is_empty() {
                    input.push(json!({ "role": "assistant", "content": [{ "type": "output_text", "text": text }] }));
                }
                if let Some(Value::Array(tool_calls)) = m.get("tool_calls") {
                    for tc in tool_calls {
                        let function = tc.get("function");
                        let mut item = Map::new();
                        item.insert("type".into(), Value::String("function_call".into()));
                        item.insert(
                            "call_id".into(),
                            shorten_codex_call_id(tc.get("id").unwrap_or(&Value::Null)),
                        );
                        if let Some(name) = function.and_then(|f| f.get("name")) {
                            item.insert("name".into(), name.clone());
                        }
                        let arguments = function.and_then(|f| f.get("arguments")).filter(|v| !v.is_null());
                        item.insert(
                            "arguments".into(),
                            arguments.cloned().unwrap_or_else(|| Value::String("{}".into())),
                        );
                        input.push(Value::Object(item));
                    }
                }
            }
            "tool" => input.push(json!({
                "type": "function_call_output",
                "call_id": shorten_codex_call_id(m.get("tool_call_id").unwrap_or(&Value::Null)),
                "output": content_text(m.get("content")),
            })),
            _ => {}
        }
    }
    input
}

/// Join every `role: "system"` message's text, in order, with a blank line.
fn extract_system_instructions(messages: &[Value]) -> String {
    messages
        .iter()
        .filter(|m| m.get("role").map(|r| js_string(Some(r))).unwrap_or_default() == "system")
        .map(|m| content_text(m.get("content")))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Pure builder for the `/codex/responses` request body. `reasoning` arrives already mapped
/// and clamped (the adapter resolves `reasoning_effort` before calling this).
pub fn build_codex_request_body(req: &ChatCompletionRequest, reasoning: Option<&Value>) -> Map<String, Value> {
    let instructions = extract_system_instructions(&req.messages);
    // System messages are hoisted into `instructions`; filter them before the input mapper so
    // its defensive system→developer branch cannot duplicate the top-level instructions item.
    let input_messages: Vec<Value> = req
        .messages
        .iter()
        .filter(|m| !m.is_object() || m.get("role").map(|r| js_string(Some(r))).unwrap_or_default() != "system")
        .cloned()
        .collect();
    // `tools: []` counts as no tools — upstream may reject tool_choice without tools.
    let has_tools = req.tools.as_ref().and_then(Value::as_array).map(|t| !t.is_empty()).unwrap_or(false);

    let mut body = Map::new();
    body.insert("model".into(), Value::String(req.upstream_model.clone()));
    body.insert("input".into(), Value::Array(openai_messages_to_codex_input(&input_messages)));
    // The codex backend is SSE-oriented.
    body.insert("stream".into(), Value::Bool(true));
    body.insert("store".into(), Value::Bool(false));
    body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    if !instructions.is_empty() {
        body.insert("instructions".into(), Value::String(instructions));
    }
    if let Some(reasoning) = reasoning {
        body.insert("reasoning".into(), reasoning.clone());
    }
    if has_tools {
        body.insert("tools".into(), Value::Array(map_tools(req.tools.as_ref())));
        body.insert("parallel_tool_calls".into(), Value::Bool(true));
    }
    if let Some(choice) = map_codex_tool_choice(req.tool_choice.as_ref(), has_tools) {
        body.insert("tool_choice".into(), choice);
    }
    if let Some(rf) = req.response_format.as_ref().and_then(Value::as_object) {
        let schema = rf.get("json_schema").and_then(|s| s.get("schema"));
        if rf.get("type").and_then(Value::as_str) == Some("json_schema") {
            if let Some(schema) = schema.filter(|s| !s.is_null()) {
                let js = rf.get("json_schema");
                let name = js
                    .and_then(|j| j.get("name"))
                    .and_then(Value::as_str)
                    .filter(|n| !n.is_empty())
                    .unwrap_or("response");
                let strict = js.and_then(|j| j.get("strict")).cloned().unwrap_or(Value::Bool(false));
                body.insert(
                    "text".into(),
                    json!({ "format": { "type": "json_schema", "name": name, "schema": schema, "strict": strict } }),
                );
            }
        }
    }
    // Both wire-bound values are fitted (here and in `codex_session_id_header`); only the
    // replay session key still uses the full client value, since it never leaves this process.
    if let Some(key) = req.prompt_cache_key.as_deref().filter(|k| !k.is_empty()) {
        body.insert("prompt_cache_key".into(), Value::String(fit_codex_prompt_cache_key(key)));
    }
    if let Some(tier) = req.raw_body.get("service_tier") {
        body.insert("service_tier".into(), tier.clone());
    }
    strip_rejected_codex_fields(body)
}

fn responses_item_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("text").and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

const CALL_ID_ITEM_TYPES: [&str; 4] =
    ["function_call", "function_call_output", "custom_tool_call", "custom_tool_call_output"];

/// The client's own Responses body, fitted for `/codex/responses` with the same fix-ups the
/// Chat path applies (docs/api.md "Native path"): system items hoisted into `instructions`,
/// `store: false` / `stream: true` forced, `include` guaranteed, effort clamped, call_ids
/// shortened, `prompt_cache_key` fitted, `tool_choice` dropped without tools, rejected fields
/// stripped. Everything else — client-echoed reasoning items, hosted tools, namespace groups,
/// `text`, `client_metadata` — rides through untouched.
pub fn build_codex_native_request_body(
    client: &Map<String, Value>,
    upstream_model: &str,
    reasoning: Option<&Value>,
) -> Map<String, Value> {
    let mut body = client.clone();
    body.insert("model".into(), Value::String(upstream_model.to_string()));
    body.insert("stream".into(), Value::Bool(true));
    body.insert("store".into(), Value::Bool(false));

    let items: Vec<Value> = match body.get("input") {
        Some(Value::String(text)) => vec![json!({ "type": "message", "role": "user", "content": text })],
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let mut system: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for item in &items {
        let Some(obj) = item.as_object() else { continue };
        let item_type = obj.get("type").and_then(Value::as_str);
        if obj.get("role").and_then(Value::as_str) == Some("system")
            && (item_type == Some("message") || obj.get("type").is_none())
        {
            let text = responses_item_text(obj.get("content"));
            if !text.is_empty() {
                system.push(text);
            }
            continue;
        }
        if item_type.map(|t| CALL_ID_ITEM_TYPES.contains(&t)).unwrap_or(false)
            && obj.get("call_id").and_then(Value::as_str).is_some()
        {
            let mut shortened = obj.clone();
            shortened.insert("call_id".into(), shorten_codex_call_id(&obj["call_id"]));
            input.push(Value::Object(shortened));
            continue;
        }
        input.push(item.clone());
    }
    let mut instructions: Vec<String> = Vec::new();
    if let Some(existing) = body.get("instructions").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        instructions.push(existing.to_string());
    }
    instructions.extend(system);
    let instructions = instructions.join("\n\n");
    if instructions.is_empty() {
        body.remove("instructions");
    } else {
        body.insert("instructions".into(), Value::String(instructions));
    }
    body.insert("input".into(), Value::Array(input));

    let mut include: Vec<Value> = body
        .get("include")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|v| v.is_string()).cloned().collect())
        .unwrap_or_default();
    if !include.iter().any(|v| v.as_str() == Some("reasoning.encrypted_content")) {
        include.push(Value::String("reasoning.encrypted_content".into()));
    }
    body.insert("include".into(), Value::Array(include));

    let client_reasoning = body.get("reasoning").and_then(Value::as_object).cloned();
    match reasoning.and_then(Value::as_object) {
        Some(mapped) => {
            let mut merged = client_reasoning.clone().unwrap_or_default();
            merged.insert("effort".into(), mapped.get("effort").cloned().unwrap_or(Value::Null));
            let summary = client_reasoning
                .as_ref()
                .and_then(|c| c.get("summary"))
                .filter(|v| !v.is_null())
                .cloned()
                .or_else(|| mapped.get("summary").cloned())
                .unwrap_or(Value::Null);
            merged.insert("summary".into(), summary);
            body.insert("reasoning".into(), Value::Object(merged));
        }
        None => {
            // Effort `none` (or unmapped): drop the effort but keep the client's other
            // reasoning settings, e.g. the summary mode the CLI asked for.
            if let Some(mut client_reasoning) = client_reasoning {
                if client_reasoning.contains_key("effort") {
                    client_reasoning.remove("effort");
                    if client_reasoning.is_empty() {
                        body.remove("reasoning");
                    } else {
                        body.insert("reasoning".into(), Value::Object(client_reasoning));
                    }
                }
            }
        }
    }

    let has_tools = body.get("tools").and_then(Value::as_array).map(|t| !t.is_empty()).unwrap_or(false);
    if !has_tools {
        body.remove("tools");
        body.remove("tool_choice");
        body.remove("parallel_tool_calls");
    }

    match body.get("prompt_cache_key").and_then(Value::as_str).filter(|k| !k.is_empty()) {
        Some(key) => {
            let fitted = fit_codex_prompt_cache_key(key);
            body.insert("prompt_cache_key".into(), Value::String(fitted));
        }
        None => {
            body.remove("prompt_cache_key");
        }
    }

    strip_rejected_codex_fields(body)
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

fn codex_request(
    access_token: &str,
    chatgpt_account_id: &str,
    session_id: &str,
    body: &Map<String, Value>,
    extras: &CallExtras,
) -> UpstreamRequest {
    let mut request = UpstreamRequest::post(CODEX_RESPONSES_URL)
        .header("authorization", &format!("Bearer {access_token}"))
        .header("user-agent", CODEX_USER_AGENT)
        .header("originator", CODEX_ORIGINATOR)
        .header("connection", "Keep-Alive")
        .header("session_id", session_id)
        .header("accept", "text/event-stream")
        .json(&Value::Object(body.clone()));
    if !chatgpt_account_id.is_empty() {
        request = request.header("chatgpt-account-id", chatgpt_account_id);
    }
    if let Some(timeout) = extras.first_byte_timeout {
        request = request.timeout(timeout);
    }
    request
}

/// Upstream non-2xx passes through with its own status and body — a `413
/// request_too_large` from the Responses backend reaches the client as-is, as it did through
/// the relay, and dispatch classifies it (docs/api.md § Errors).
async fn passthrough_error(response: UpstreamResponse) -> Response {
    let status = response.status;
    let content_type = response.content_type().unwrap_or("application/json").to_string();
    let body = response.bytes().await.unwrap_or_default();
    let mut out = Response::new(Body::from(body));
    *out.status_mut() = status;
    if let Ok(value) = http::HeaderValue::from_str(&content_type) {
        out.headers_mut().insert(http::header::CONTENT_TYPE, value);
    }
    out
}

fn json_response(status: StatusCode, value: &Value) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut out = Response::new(Body::from(body));
    *out.status_mut() = status;
    out.headers_mut().insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
    out
}

fn sse_response(body: Body) -> Response {
    let mut out = Response::new(body);
    *out.status_mut() = StatusCode::OK;
    let headers = out.headers_mut();
    headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("text/event-stream; charset=utf-8"));
    headers.insert(http::header::CACHE_CONTROL, http::HeaderValue::from_static("no-cache"));
    out
}

// ---------------------------------------------------------------------------
// Reasoning replay wiring
// ---------------------------------------------------------------------------

/// Everything the completed-turn callback needs to persist a replay record. Retains all
/// matched earlier turns, including after a no-reasoning completion.
struct ReplayWriter {
    cx: AppState,
    api_key_id: String,
    model: String,
    session_key: String,
    start_hash: String,
    history: CodexReasoningReplayEntry,
}

impl ReplayWriter {
    async fn write(&self, items: Vec<Value>, assistant_text: String) {
        let turn = codex_replay_turn(&self.start_hash, &items, &assistant_text, &shorten_codex_call_id);
        let history = append_codex_replay_turn(&self.history, turn);
        if history.turns.is_empty() {
            return;
        }
        write_codex_reasoning_replay(
            self.cx.cache(),
            &self.api_key_id,
            &self.model,
            Some(&self.session_key),
            &history,
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

pub struct CodexAdapter;

pub fn adapter() -> DynAdapter {
    Arc::new(CodexAdapter)
}

impl CodexAdapter {
    /// Native Responses ingress: same identity headers and error handling as
    /// `chat_completions`, but the upstream Responses SSE is returned as-is (stream) or
    /// collected into its terminal `response` object (non-stream). The reasoning replay cache
    /// is deliberately not involved — a Responses client echoes its own reasoning items
    /// (docs/providers.md § Codex).
    async fn native_responses(
        &self,
        cx: &AppState,
        credential: &StoredCredential,
        chatgpt_account_id: &str,
        req: &ChatCompletionRequest,
        extras: &CallExtras,
        reasoning: Option<&Value>,
    ) -> Result<Response, AdapterError> {
        let client = req.responses_body.clone().unwrap_or_default();
        let body = build_codex_native_request_body(&client, &req.upstream_model, reasoning);
        let session_id = codex_session_id_header(req.affinity.as_ref(), req.prompt_cache_key.as_deref());
        let request =
            codex_request(&credential.access_token, chatgpt_account_id, &session_id, &body, extras);
        let response = cx.transport().send(request).await?;
        if !response.status.is_success() {
            return Ok(passthrough_error(response).await);
        }
        if req.stream.unwrap_or(false) {
            return Ok(sse_response(Body::from_stream(response.body)));
        }
        Ok(match collect_responses_sse(response.body).await {
            CollectedResponses::Response(response) => json_response(StatusCode::OK, &response),
            error => json_response(
                StatusCode::BAD_GATEWAY,
                &error.error_value().unwrap_or(Value::Null),
            ),
        })
    }
}

#[async_trait]
impl ProviderAdapter for CodexAdapter {
    fn id(&self) -> &str {
        "codex"
    }

    async fn refresh_if_needed(&self, cx: &AppState, account: AcquiredAccount) -> Result<AcquiredAccount, AdapterError> {
        Ok(refresh_codex(cx, account).await)
    }

    fn has_list_models(&self) -> bool {
        true
    }

    async fn list_models(&self, cx: &AppState, account: &AcquiredAccount) -> ListedModels {
        let acc = refresh_codex(cx, account.clone()).await;
        let account_id = chatgpt_account_id(&acc.credential);
        fetch_codex_models(cx, &acc.credential.access_token, &account_id).await
    }

    fn has_fetch_usage(&self) -> bool {
        true
    }

    async fn fetch_usage(&self, cx: &AppState, account: &AcquiredAccount) -> FetchedUsage {
        let acc = refresh_codex(cx, account.clone()).await;
        let account_id = chatgpt_account_id(&acc.credential);
        let result = fetch_codex_usage_json(cx, &acc.credential.access_token, &account_id).await;
        let account_value = |email: Option<String>, plan: Option<Option<String>>| {
            let mut map = Map::new();
            map.insert("email".into(), email.map(Value::String).unwrap_or(Value::Null));
            if let Some(plan) = plan {
                map.insert("plan_type".into(), plan.map(Value::String).unwrap_or(Value::Null));
            }
            map.insert(
                "account_id".into(),
                if account_id.is_empty() { Value::Null } else { Value::String(account_id.clone()) },
            );
            map
        };

        if let (true, Some(payload)) = (result.ok, result.payload.as_ref()) {
            return FetchedUsage {
                windows: windows_from_codex_payload(payload),
                account: account_value(
                    payload.email.clone().or_else(|| acc.credential.email.clone()),
                    Some(payload.plan_type.clone()),
                ),
                stale: false,
                error: None,
                edge_blocked: false,
            };
        }

        // The usage endpoint is bot-walled for some accounts; the account is still usable.
        FetchedUsage {
            windows: Vec::new(),
            account: account_value(acc.credential.email.clone(), None),
            stale: true,
            error: result.error,
            edge_blocked: result.edge_blocked,
        }
    }

    async fn chat_completions(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        req: &ChatCompletionRequest,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let acc = refresh_codex(cx, account.clone()).await;
        let mapped = map_reasoning(ProviderId::Codex, req.reasoning_effort);
        let reasoning = mapped.get("reasoning").cloned();
        let account_id = chatgpt_account_id(&acc.credential);

        if req.responses_body.is_some() {
            return self
                .native_responses(cx, &acc.credential, &account_id, req, extras, reasoning.as_ref())
                .await;
        }

        let api_key_id = extras.api_key_id.clone().unwrap_or_default();
        let client_session =
            codex_reasoning_replay_session_key(req.affinity.as_ref(), req.prompt_cache_key.as_deref());
        // Opaque reasoning belongs to the upstream account as well as the conversation.
        let session_key = client_session.map(|session| format!("{session}\0{}", account.row.id));
        let replay_scoped = !api_key_id.is_empty() && session_key.is_some();

        let mut body = build_codex_request_body(req, reasoning.as_ref());
        let hashes = if replay_scoped { codex_input_hashes(&body) } else { Vec::new() };
        let history = if replay_scoped {
            read_codex_reasoning_replay(cx.cache(), &api_key_id, &req.upstream_model, session_key.as_deref()).await
        } else {
            None
        };
        let input: Vec<Value> =
            body.get("input").and_then(Value::as_array).cloned().unwrap_or_default();
        let (replayed, retained) = replay_codex_history(&input, &hashes, history.as_ref());
        body.insert("input".into(), Value::Array(replayed));

        let writer = if replay_scoped {
            Some(Arc::new(ReplayWriter {
                cx: cx.clone(),
                api_key_id,
                model: req.upstream_model.clone(),
                session_key: session_key.clone().unwrap_or_default(),
                start_hash: hashes.last().cloned().unwrap_or_default(),
                history: retained,
            }))
        } else {
            None
        };

        let session_id = codex_session_id_header(req.affinity.as_ref(), req.prompt_cache_key.as_deref());
        let request =
            codex_request(&acc.credential.access_token, &account_id, &session_id, &body, extras);
        let response = cx.transport().send(request).await?;
        if !response.status.is_success() {
            return Ok(passthrough_error(response).await);
        }

        if req.stream.unwrap_or(false) {
            // `waitUntil` becomes a detached task: the client is already draining the stream
            // when the completed turn is known, so the cache write cannot be awaited inline.
            let opts = match writer {
                Some(writer) => CodexSseOptions::with_replay_items(Arc::new(move |items, text| {
                    let writer = writer.clone();
                    tokio::spawn(async move { writer.write(items, text).await });
                })),
                None => CodexSseOptions::default(),
            };
            let stream = codex_sse_to_openai_stream(response.body, req.upstream_model.clone(), opts);
            return Ok(sse_response(Body::from_stream(stream)));
        }

        // Non-stream: consume the SSE and build one completion. The replay write is awaited
        // here, before the response is handed back.
        let captured: Arc<Mutex<Option<(Vec<Value>, String)>>> = Arc::new(Mutex::new(None));
        let opts = if writer.is_some() {
            let slot = captured.clone();
            CodexSseOptions::with_replay_items(Arc::new(move |items, text| {
                *slot.lock().expect("replay slot is not poisoned") = Some((items, text));
            }))
        } else {
            CodexSseOptions::default()
        };
        let collected = collect_codex_sse(response.body, &req.upstream_model, opts).await;
        if let Some(writer) = writer {
            let captured = captured.lock().expect("replay slot is not poisoned").take();
            if let Some((items, text)) = captured {
                writer.write(items, text).await;
            }
        }
        Ok(match collected {
            CodexCollected::Completion(completion) => json_response(StatusCode::OK, &completion),
            // response.failed / error mid-turn: never fabricate a 200 completion.
            error => json_response(StatusCode::BAD_GATEWAY, &error.error_value().unwrap_or(Value::Null)),
        })
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_account, insert_user, skip_without_db, test_pool, test_state};
    use crate::providers::codex_models::CODEX_CLIENT_VERSION;
    use crate::providers::codex_reasoning_cache::codex_reasoning_replay_cache_key_for_test;
    use crate::proxy::openai_anthropic::anthropic_to_openai_chat_request;
    use crate::upstream::transport::RecordedRequest;
    use crate::upstream::MockTransport;
    use serde_json::json;
    use std::time::Duration;

    // ----- fixtures -------------------------------------------------------

    fn request(messages: Value) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "codex/gpt-5.2".into(),
            raw_model: "codex/gpt-5.2".into(),
            upstream_model: "m".into(),
            messages: messages.as_array().cloned().unwrap_or_default(),
            ..Default::default()
        }
    }

    fn build(messages: Value) -> Map<String, Value> {
        build_codex_request_body(&request(messages), None)
    }

    fn input_of(body: &Map<String, Value>) -> Vec<Value> {
        body.get("input").and_then(Value::as_array).cloned().unwrap_or_default()
    }

    fn affinity(session: Option<&str>, conv: Option<&str>) -> AffinityIds {
        AffinityIds {
            session_id: session.map(str::to_string),
            conv_id: conv.map(str::to_string),
            turn_idx: None,
        }
    }

    async fn codex_fixture(mock: &Arc<MockTransport>) -> Option<(AppState, AcquiredAccount)> {
        let pool = test_pool().await?;
        let cx = test_state(pool.clone(), mock.clone());
        let user = insert_user(&pool, "codex@example.com").await;
        let credential = StoredCredential { access_token: "tok_test".into(), ..Default::default() };
        let row = insert_account(&pool, &user.id, "codex", &credential).await;
        Some((cx, AcquiredAccount { row, credential }))
    }

    /// Drives the real adapter path down to the stubbed transport and returns the outbound
    /// request — asserting on `build_codex_request_body` alone is what once let an unfitted
    /// `session_id` header ship green (docs/providers.md § Codex).
    async fn capture(cx: &AppState, mock: &Arc<MockTransport>, account: &AcquiredAccount, req: &ChatCompletionRequest) -> RecordedRequest {
        mock.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::OK, http::HeaderMap::new(), "")));
        let response = CodexAdapter.chat_completions(cx, account, req, &CallExtras::default()).await.unwrap();
        let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        mock.requests().last().cloned().expect("one recorded request")
    }

    // ----- upstream request headers ---------------------------------------

    fn header_req() -> ChatCompletionRequest {
        ChatCompletionRequest {
            upstream_model: "gpt-5.2".into(),
            ..request(json!([{ "role": "user", "content": "hi" }]))
        }
    }

    #[tokio::test]
    async fn uses_the_cli_identity_headers_and_omits_openai_beta() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let captured = capture(&cx, &mock, &account, &header_req()).await;
        assert_eq!(captured.url, CODEX_RESPONSES_URL);
        assert_eq!(captured.header("user-agent"), Some(CODEX_USER_AGENT));
        assert_eq!(captured.header("originator"), Some(CODEX_ORIGINATOR));
        assert_eq!(captured.header("connection"), Some("Keep-Alive"));
        assert_eq!(captured.header("openai-beta"), None);
        assert!(!captured.header("session_id").unwrap_or("").is_empty());
        assert_eq!(captured.header("authorization"), Some("Bearer tok_test"));
        // Direct egress: no relay envelope survives in the Rust edition.
        assert_eq!(captured.header("x-serverless-authorization"), None);
    }

    #[tokio::test]
    async fn uses_session_affinity_first_then_conversation_affinity() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let req = ChatCompletionRequest { affinity: Some(affinity(Some("session-1"), Some("conv-1"))), ..header_req() };
        assert_eq!(capture(&cx, &mock, &account, &req).await.header("session_id"), Some("session-1"));
        let req = ChatCompletionRequest { affinity: Some(affinity(None, Some("conv-2"))), ..header_req() };
        assert_eq!(capture(&cx, &mock, &account, &req).await.header("session_id"), Some("conv-2"));
    }

    #[tokio::test]
    async fn derives_session_id_from_prompt_cache_key_when_no_affinity_headers_exist() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let uuid = "0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e";
        let req = ChatCompletionRequest {
            prompt_cache_key: Some(format!("user_ab_account_11111111-2222-3333-4444-555555555555_session_{uuid}")),
            ..header_req()
        };
        assert_eq!(capture(&cx, &mock, &account, &req).await.header("session_id"), Some(uuid));

        let req = ChatCompletionRequest { prompt_cache_key: Some("conv-key-1".into()), ..header_req() };
        assert_eq!(capture(&cx, &mock, &account, &req).await.header("session_id"), Some("conv-key-1"));

        let req = ChatCompletionRequest {
            prompt_cache_key: Some("conv-key-1".into()),
            affinity: Some(affinity(Some("session-9"), None)),
            ..header_req()
        };
        assert_eq!(capture(&cx, &mock, &account, &req).await.header("session_id"), Some("session-9"));
    }

    #[tokio::test]
    async fn fits_an_over_long_prompt_cache_key_in_the_session_id_header_too() {
        // The backend validates this header under the `prompt_cache_key` name, so an
        // unfitted header 400s the turn even with a fitted body field.
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let req = ChatCompletionRequest { prompt_cache_key: Some("q".repeat(200)), ..header_req() };
        let captured = capture(&cx, &mock, &account, &req).await;
        let session_id = captured.header("session_id").unwrap();
        assert!(session_id.len() == 64 && session_id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        let body_key = captured.json()["prompt_cache_key"].as_str().unwrap().to_string();
        assert_eq!(body_key, session_id);
    }

    #[tokio::test]
    async fn fits_an_over_long_client_affinity_id_in_the_session_id_header() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let req = ChatCompletionRequest { affinity: Some(affinity(Some(&"s".repeat(120)), None)), ..header_req() };
        let session_id = capture(&cx, &mock, &account, &req).await.header("session_id").unwrap().to_string();
        assert!(session_id.len() == 64 && session_id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn omits_an_empty_chatgpt_account_id() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        assert_eq!(capture(&cx, &mock, &account, &header_req()).await.header("chatgpt-account-id"), None);
    }

    #[tokio::test]
    async fn sends_the_chatgpt_account_id_from_the_credential_or_the_token_claims() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let stored = AcquiredAccount {
            credential: StoredCredential { account_id: Some("acct-stored".into()), ..account.credential.clone() },
            ..account.clone()
        };
        assert_eq!(capture(&cx, &mock, &stored, &header_req()).await.header("chatgpt-account-id"), Some("acct-stored"));

        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-jwt"}}"#);
        let from_jwt = AcquiredAccount {
            credential: StoredCredential { access_token: format!("h.{claims}.s"), ..Default::default() },
            ..account.clone()
        };
        assert_eq!(capture(&cx, &mock, &from_jwt, &header_req()).await.header("chatgpt-account-id"), Some("acct-jwt"));
    }

    // ----- buildCodexRequestBody ------------------------------------------

    #[test]
    fn preserves_image_detail_without_adding_a_default() {
        for detail in [Some("low"), Some("high"), Some("auto"), None] {
            let mut image = json!({ "url": "https://example.com/image.png" });
            if let Some(detail) = detail {
                image["detail"] = json!(detail);
            }
            let body = build(json!([{ "role": "user", "content": [{ "type": "image_url", "image_url": image }] }]));
            let mut expected = json!({ "type": "input_image", "image_url": "https://example.com/image.png" });
            if let Some(detail) = detail {
                expected["detail"] = json!(detail);
            }
            assert_eq!(input_of(&body), vec![json!({ "role": "user", "content": [expected] })]);
        }
    }

    #[test]
    fn always_includes_encrypted_reasoning_and_only_enables_parallel_tools_with_tools() {
        let no_tools = build(json!([{ "role": "user", "content": "hi" }]));
        assert_eq!(no_tools["include"], json!(["reasoning.encrypted_content"]));
        assert!(!no_tools.contains_key("parallel_tool_calls"));

        let req = ChatCompletionRequest {
            tools: Some(json!([{ "type": "function", "function": { "name": "lookup", "parameters": {} } }])),
            ..request(json!([{ "role": "user", "content": "hi" }]))
        };
        let with_tools = build_codex_request_body(&req, None);
        assert_eq!(with_tools["parallel_tool_calls"], json!(true));
        assert_eq!(
            with_tools["tools"],
            json!([{ "type": "function", "name": "lookup", "parameters": {} }])
        );
    }

    #[test]
    fn strips_rejected_fields_and_keeps_only_the_priority_service_tier() {
        let mut raw = Map::new();
        for field in CODEX_REJECTED_BODY_FIELDS {
            raw.insert(field.into(), json!("reject"));
        }
        raw.insert("service_tier".into(), json!("auto"));
        let req = ChatCompletionRequest { raw_body: raw, ..request(json!([{ "role": "user", "content": "hi" }])) };
        let body = build_codex_request_body(&req, None);
        for field in CODEX_REJECTED_BODY_FIELDS {
            assert!(!body.contains_key(field), "{field} must not reach upstream");
        }
        assert!(!body.contains_key("service_tier"));

        let req = ChatCompletionRequest {
            raw_body: json!({ "service_tier": "priority" }).as_object().cloned().unwrap(),
            ..request(json!([{ "role": "user", "content": "hi" }]))
        };
        assert_eq!(build_codex_request_body(&req, None)["service_tier"], json!("priority"));
    }

    #[test]
    fn rewrites_a_system_item_that_reaches_input_to_developer() {
        let body = build(json!([{ "role": "system", "content": "rules" }]));
        assert_eq!(body["instructions"], json!("rules"));
        assert!(!input_of(&body).iter().any(|item| item.get("role") == Some(&json!("system"))));
        // The defensive mapper path, reached only from another caller.
        assert_eq!(
            openai_messages_to_codex_input(&[json!({ "role": "system", "content": "rules" })]),
            vec![json!({ "role": "developer", "content": [{ "type": "input_text", "text": "rules" }] })]
        );
    }

    #[test]
    fn shortens_long_call_ids_consistently_and_preserves_short_ids() {
        let long_id = "a".repeat(80);
        let expected = format!("{}_0f45e858fbc4176c", "a".repeat(47));
        let body = build(json!([
            { "role": "assistant", "content": null, "tool_calls": [{ "id": long_id, "function": { "name": "lookup", "arguments": "{}" } }] },
            { "role": "tool", "tool_call_id": long_id, "content": "ok" },
            { "role": "assistant", "content": null, "tool_calls": [{ "id": "short", "function": { "name": "x" } }] },
        ]));
        let input = input_of(&body);
        assert_eq!(input[0]["call_id"], json!(expected));
        assert_eq!(input[1]["call_id"], json!(expected));
        assert_eq!(input[2]["call_id"], json!("short"));
        assert_eq!(expected.len(), 64);
    }

    #[test]
    fn matches_the_reference_sha256_suffix_for_a_long_call_id() {
        let body = build(json!([
            { "role": "assistant", "content": null, "tool_calls": [{ "id": "x".repeat(65), "function": { "name": "x" } }] }
        ]));
        assert_eq!(input_of(&body)[0]["call_id"], json!(format!("{}_9537c5fdf120482f", "x".repeat(47))));
    }

    #[test]
    fn leaves_exactly_64_character_call_ids_untouched() {
        let id = "z".repeat(64);
        let body = build(json!([
            { "role": "assistant", "content": null, "tool_calls": [{ "id": id, "function": { "name": "x" } }] }
        ]));
        assert_eq!(input_of(&body)[0]["call_id"], json!(id));
    }

    #[test]
    fn derives_the_call_id_suffix_from_a_real_sha256() {
        // Pin the suffix to the platform digest, including a multi-byte input, so the id a
        // client sends still resolves to the same shortened form.
        for id in ["z".repeat(65), "汉字".repeat(50), "a".repeat(120), format!("toolu_{}", "x".repeat(100))] {
            let digest = hex::encode(Sha256::digest(id.as_bytes()));
            let suffix = format!("_{}", &digest[..16]);
            let head: String = id.chars().take(64 - suffix.len()).collect();
            let body = build(json!([
                { "role": "assistant", "tool_calls": [{ "id": id, "function": { "name": "f", "arguments": "{}" } }] }
            ]));
            let got = input_of(&body)[0]["call_id"].as_str().unwrap().to_string();
            assert_eq!(got, format!("{head}{suffix}"));
            assert_eq!(got.chars().count(), 64);
        }
    }

    #[test]
    fn gives_a_function_call_and_its_output_the_same_shortened_id() {
        // If these diverge the tool result no longer pairs with its call upstream.
        let id = format!("call_{}", "9".repeat(120));
        let body = build(json!([
            { "role": "assistant", "tool_calls": [{ "id": id, "function": { "name": "f", "arguments": "{}" } }] },
            { "role": "tool", "tool_call_id": id, "content": "ok" },
        ]));
        let input = input_of(&body);
        let call = input.iter().find(|i| i["type"] == json!("function_call")).unwrap();
        let output = input.iter().find(|i| i["type"] == json!("function_call_output")).unwrap();
        assert_eq!(call["call_id"], output["call_id"]);
        assert_eq!(call["call_id"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn sends_store_false_unconditionally() {
        assert_eq!(build(json!([{ "role": "user", "content": "hi" }]))["store"], json!(false));
    }

    #[test]
    fn folds_system_messages_into_instructions_and_drops_them_from_input() {
        let body = build(json!([
            { "role": "system", "content": "Be terse." },
            { "role": "user", "content": "hi" },
        ]));
        assert_eq!(body["instructions"], json!("Be terse."));
        let input = input_of(&body);
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], json!("user"));
        // No trace of the old "[system]\n" fake-user-message wrapping.
        assert!(!Value::Array(input).to_string().contains("[system]"));

        let body = build(json!([
            { "role": "system", "content": "First." },
            { "role": "user", "content": "hi" },
            { "role": "system", "content": "Second." },
        ]));
        assert_eq!(body["instructions"], json!("First.\n\nSecond."));
        assert_eq!(input_of(&body).len(), 1);

        assert!(!build(json!([{ "role": "user", "content": "hi" }])).contains_key("instructions"));
    }

    #[test]
    fn maps_tool_choice_to_the_responses_shape() {
        let tools = json!([{ "type": "function", "function": { "name": "lookup", "parameters": {} } }]);
        let with = |tool_choice: Option<Value>, tools: Option<Value>| {
            build_codex_request_body(
                &ChatCompletionRequest {
                    tools,
                    tool_choice,
                    ..request(json!([{ "role": "user", "content": "hi" }]))
                },
                None,
            )
        };
        for choice in ["auto", "none", "required"] {
            assert_eq!(with(Some(json!(choice)), Some(tools.clone()))["tool_choice"], json!(choice));
        }
        assert_eq!(
            with(Some(json!({ "type": "function", "function": { "name": "lookup" } })), Some(tools.clone()))["tool_choice"],
            json!({ "type": "function", "name": "lookup" })
        );
        assert_eq!(with(None, Some(tools))["tool_choice"], json!("auto"));

        for tools in [None, Some(json!([]))] {
            let body = with(Some(json!("auto")), tools);
            assert!(!body.contains_key("tool_choice"));
            assert!(!body.contains_key("tools"));
        }
    }

    #[test]
    fn forwards_and_fits_the_prompt_cache_key() {
        let with_key = |key: Option<&str>| {
            build_codex_request_body(
                &ChatCompletionRequest {
                    prompt_cache_key: key.map(str::to_string),
                    ..request(json!([{ "role": "user", "content": "hi" }]))
                },
                None,
            )
        };
        assert_eq!(with_key(Some("conv-123"))["prompt_cache_key"], json!("conv-123"));
        assert!(!with_key(None).contains_key("prompt_cache_key"));

        // Upstream rejects anything over 64 chars with `Invalid 'prompt_cache_key'`.
        let uuid = "0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e";
        let key = format!("user_{}_account_11111111-2222-3333-4444-555555555555_session_{uuid}", "a".repeat(64));
        assert!(key.len() > 140);
        assert_eq!(with_key(Some(&key))["prompt_cache_key"], json!(uuid));

        let long = "x".repeat(200);
        let first = with_key(Some(&long))["prompt_cache_key"].clone();
        assert_eq!(first, with_key(Some(&long))["prompt_cache_key"]);
        let hex_key = first.as_str().unwrap();
        assert!(hex_key.len() == 64 && hex_key.chars().all(|c| c.is_ascii_hexdigit()));

        // Two long keys sharing a prefix must land on distinct cache shards.
        let prefix = format!("{}{}", "user_shared_account_prefix", "p".repeat(74));
        assert_ne!(
            with_key(Some(&format!("{prefix}-alpha")))["prompt_cache_key"],
            with_key(Some(&format!("{prefix}-beta")))["prompt_cache_key"]
        );

        for key in ["short".to_string(), "y".repeat(64), "y".repeat(65), "z".repeat(300), format!("user_ab_account_1_session_{uuid}")] {
            assert!(with_key(Some(&key))["prompt_cache_key"].as_str().unwrap().len() <= 64);
        }
    }

    #[test]
    fn sets_reasoning_when_passed_and_omits_it_otherwise() {
        let with = build_codex_request_body(
            &request(json!([{ "role": "user", "content": "hi" }])),
            Some(&json!({ "effort": "high", "summary": "auto" })),
        );
        assert_eq!(with["reasoning"], json!({ "effort": "high", "summary": "auto" }));
        assert!(!build(json!([{ "role": "user", "content": "hi" }])).contains_key("reasoning"));
    }

    #[test]
    fn emits_the_assistant_text_message_before_its_function_call_items() {
        let body = build(json!([{
            "role": "assistant",
            "content": "Let me check that file.",
            "tool_calls": [{ "id": "call_1", "type": "function", "function": { "name": "Read", "arguments": "{\"file_path\":\"/a\"}" } }],
        }]));
        assert_eq!(
            input_of(&body),
            vec![
                json!({ "role": "assistant", "content": [{ "type": "output_text", "text": "Let me check that file." }] }),
                json!({ "type": "function_call", "call_id": "call_1", "name": "Read", "arguments": "{\"file_path\":\"/a\"}" }),
            ]
        );
    }

    #[test]
    fn keeps_only_function_call_items_when_there_is_no_assistant_text() {
        let body = build(json!([{
            "role": "assistant",
            "content": null,
            "tool_calls": [
                { "id": "call_1", "type": "function", "function": { "name": "A", "arguments": "{}" } },
                { "id": "call_2", "type": "function", "function": { "name": "B", "arguments": "{}" } },
            ],
        }]));
        let input = input_of(&body);
        assert_eq!(input.len(), 2);
        assert!(input.iter().all(|i| i["type"] == json!("function_call")));
    }

    #[test]
    fn emits_only_the_text_message_when_there_are_no_tool_calls() {
        let body = build(json!([{ "role": "assistant", "content": "just text" }]));
        assert_eq!(
            input_of(&body),
            vec![json!({ "role": "assistant", "content": [{ "type": "output_text", "text": "just text" }] })]
        );
    }

    // ----- Anthropic ingress (via anthropicToOpenAIChatRequest) -----------

    fn anthropic_messages(body: Value) -> Vec<Value> {
        anthropic_to_openai_chat_request(body.as_object().unwrap()).messages
    }

    #[test]
    fn anthropic_system_ends_up_in_instructions_not_input() {
        let messages = anthropic_messages(json!({
            "system": "You are a helpful assistant.",
            "messages": [{ "role": "user", "content": "hi" }],
        }));
        let body = build_codex_request_body(
            &ChatCompletionRequest { upstream_model: "gpt-5.2".into(), messages, ..Default::default() },
            None,
        );
        assert_eq!(body["instructions"], json!("You are a helpful assistant."));
        assert!(!input_of(&body).iter().any(|m| m.get("role") == Some(&json!("system"))));

        let messages = anthropic_messages(json!({
            "system": [{ "type": "text", "text": "Part one." }, { "type": "text", "text": "Part two." }],
            "messages": [{ "role": "user", "content": "hi" }],
        }));
        let body = build_codex_request_body(
            &ChatCompletionRequest { upstream_model: "gpt-5.2".into(), messages, ..Default::default() },
            None,
        );
        assert_eq!(body["instructions"], json!("Part one.\n\nPart two."));
    }

    #[test]
    fn reasoning_content_on_replayed_history_is_silently_ignored() {
        let messages = anthropic_messages(json!({
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "reasoning from a prior turn" },
                    { "type": "text", "text": "here is the answer" },
                ] },
                { "role": "user", "content": "thanks" },
            ],
        }));
        assert_eq!(messages[0]["reasoning_content"], json!("reasoning from a prior turn"));
        let body = build_codex_request_body(
            &ChatCompletionRequest { upstream_model: "gpt-5.2".into(), messages, ..Default::default() },
            None,
        );
        let serialized = Value::Object(body.clone()).to_string();
        assert!(!serialized.contains("reasoning_content"));
        assert!(!serialized.contains("reasoning from a prior turn"));
        assert_eq!(
            input_of(&body)[0],
            json!({ "role": "assistant", "content": [{ "type": "output_text", "text": "here is the answer" }] })
        );
    }

    // ----- codexSessionId --------------------------------------------------

    #[test]
    fn codex_session_id_extracts_the_bare_session_uuid() {
        let uuid = "0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e";
        assert_eq!(codex_session_id(Some(&format!("user_ab_account_x_session_{uuid}"))).as_deref(), Some(uuid));
    }

    #[test]
    fn codex_session_id_passes_other_keys_through_and_yields_none_for_blanks() {
        assert_eq!(codex_session_id(Some("conv-key-1")).as_deref(), Some("conv-key-1"));
        assert_eq!(codex_session_id(Some("ends_session_notauuid")).as_deref(), Some("ends_session_notauuid"));
        assert_eq!(codex_session_id(Some("  ")), None);
        assert_eq!(codex_session_id(None), None);
    }

    // ----- codex reasoning replay wiring ----------------------------------

    /// Drives one turn whose upstream SSE reports `output`, and returns the body actually
    /// sent upstream.
    async fn run_turn(
        cx: &AppState,
        mock: &Arc<MockTransport>,
        account: &AcquiredAccount,
        req: &ChatCompletionRequest,
        output: Value,
        text: &str,
    ) -> Map<String, Value> {
        let sse = format!(
            "data: {}\n\ndata: {}\n\n",
            json!({ "type": "response.output_text.delta", "delta": text }),
            json!({ "type": "response.completed", "response": { "output": output } }),
        );
        mock.expect(move |_| Ok(UpstreamResponse::sse(StatusCode::OK, sse.clone())));
        let extras = CallExtras { api_key_id: Some("key_1".into()), ..Default::default() };
        let response = CodexAdapter.chat_completions(cx, account, req, &extras).await.unwrap();
        let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        if req.stream.unwrap_or(false) {
            // The streaming path persists replay from a detached task (the `waitUntil` seam),
            // so the client is never made to wait for a cache write.
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        mock.requests().last().unwrap().json().as_object().cloned().unwrap()
    }

    fn reasoning_item() -> Value {
        json!({ "type": "reasoning", "encrypted_content": "gpt_abc" })
    }

    fn replay_req() -> ChatCompletionRequest {
        ChatCompletionRequest {
            upstream_model: "gpt-5.6-sol".into(),
            affinity: Some(affinity(Some("sess_1"), None)),
            ..request(json!([{ "role": "user", "content": "hi" }]))
        }
    }

    fn with_messages(req: &ChatCompletionRequest, messages: Value) -> ChatCompletionRequest {
        ChatCompletionRequest { messages: messages.as_array().cloned().unwrap_or_default(), ..req.clone() }
    }

    /// The stored history for one session, read the way the adapter reads it.
    async fn stored(cx: &AppState, account: &AcquiredAccount, session: &str) -> Option<CodexReasoningReplayEntry> {
        let key = format!("{session}\0{}", account.row.id);
        read_codex_reasoning_replay(cx.cache(), "key_1", "gpt-5.6-sol", Some(&key)).await
    }

    fn input_items(body: &Map<String, Value>) -> Vec<Value> {
        body.get("input").and_then(Value::as_array).cloned().unwrap_or_default()
    }

    #[tokio::test]
    async fn persists_reasoning_from_a_completed_turn_and_replays_it_on_the_next() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let req = replay_req();
        run_turn(&cx, &mock, &account, &req, json!([reasoning_item(), { "type": "message", "content": [] }]), "answer").await;
        assert!(stored(&cx, &account, "sess_1").await.is_some());

        // The next turn echoes the prior assistant text, so the fingerprint matches.
        let next = with_messages(
            &req,
            json!([
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "answer" },
                { "role": "user", "content": "again" },
            ]),
        );
        let sent = run_turn(&cx, &mock, &account, &next, json!([]), "answer").await;
        assert!(input_items(&sent).contains(&reasoning_item()));
    }

    #[tokio::test]
    async fn does_not_replay_when_the_trailing_assistant_text_no_longer_matches() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let req = replay_req();
        run_turn(&cx, &mock, &account, &req, json!([reasoning_item()]), "answer").await;
        let next = with_messages(
            &req,
            json!([
                { "role": "assistant", "content": "a DIFFERENT answer" },
                { "role": "user", "content": "again" },
            ]),
        );
        let sent = run_turn(&cx, &mock, &account, &next, json!([]), "answer").await;
        assert!(!input_items(&sent).contains(&reasoning_item()));
    }

    #[tokio::test]
    async fn replay_is_a_no_op_without_a_session_id_or_an_api_key_id() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let req = ChatCompletionRequest { affinity: None, ..replay_req() };
        run_turn(&cx, &mock, &account, &req, json!([reasoning_item()]), "answer").await;
        assert!(stored(&cx, &account, "sess_1").await.is_none());

        // An api-key-less caller (no per-caller scope) writes nothing either.
        let req = replay_req();
        mock.expect(|_| {
            Ok(UpstreamResponse::sse(
                StatusCode::OK,
                "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"reasoning\"}]}}\n\n",
            ))
        });
        let response = CodexAdapter.chat_completions(&cx, &account, &req, &CallExtras::default()).await.unwrap();
        let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(stored(&cx, &account, "sess_1").await.is_none());
    }

    #[tokio::test]
    async fn scopes_replay_by_prompt_cache_key_when_no_affinity_headers_exist() {
        // The /anthropic Claude Code path: metadata.user_id becomes the prompt_cache_key.
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let key = "user_ab_account_1_session_0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e";
        let req = ChatCompletionRequest {
            affinity: None,
            prompt_cache_key: Some(key.into()),
            ..replay_req()
        };
        run_turn(&cx, &mock, &account, &req, json!([reasoning_item(), { "type": "message", "content": [] }]), "answer").await;
        assert!(stored(&cx, &account, key).await.is_some());

        let next = with_messages(
            &req,
            json!([
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "answer" },
                { "role": "user", "content": "again" },
            ]),
        );
        let sent = run_turn(&cx, &mock, &account, &next, json!([]), "answer").await;
        assert!(input_items(&sent).contains(&reasoning_item()));
    }

    async fn keeps_prior_prefixes(stream: bool) {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let call = |id: &str| json!({ "type": "function_call", "call_id": id, "name": "read", "arguments": "{\"a\":1,\"b\":2}" });
        let assistant = |id: &str| json!({ "role": "assistant", "content": "", "tool_calls": [
            { "id": id, "type": "function", "function": { "name": "read", "arguments": "{ \"b\": 2, \"a\": 1 }" } }
        ] });
        let result = |id: &str| json!({ "role": "tool", "tool_call_id": id, "content": "result" });
        let first_reasoning = json!({ "type": "reasoning", "encrypted_content": "first-opaque" });
        let second_reasoning = json!({ "type": "reasoning", "encrypted_content": "second-opaque" });
        let initial = vec![
            json!({ "role": "system", "content": "stable instructions" }),
            json!({ "role": "user", "content": "old task" }),
            assistant("old"),
            result("old"),
            json!({ "role": "user", "content": "new task" }),
        ];
        let base = ChatCompletionRequest { stream: Some(stream), ..replay_req() };
        let req = with_messages(&base, Value::Array(initial.clone()));

        let first = run_turn(&cx, &mock, &account, &req, json!([first_reasoning, call("one")]), "").await;
        let first_input = input_items(&first);

        let mut second_messages = initial.clone();
        second_messages.extend([assistant("one"), result("one")]);
        let second = run_turn(
            &cx,
            &mock,
            &account,
            &with_messages(&base, Value::Array(second_messages.clone())),
            json!([second_reasoning, call("two")]),
            "",
        )
        .await;
        let second_input = input_items(&second);
        assert_eq!(second_input[..first_input.len()], first_input[..]);
        assert_eq!(second_input[first_input.len()], first_reasoning);
        assert_eq!(second_input[first_input.len() + 1], call("one"));

        let mut third_messages = second_messages.clone();
        third_messages.extend([assistant("two"), result("two")]);
        let third = run_turn(
            &cx,
            &mock,
            &account,
            &with_messages(&base, Value::Array(third_messages.clone())),
            json!([]),
            "done",
        )
        .await;
        let third_input = input_items(&third);
        assert_eq!(third_input[..second_input.len()], second_input[..]);
        assert_eq!(third_input[second_input.len()], second_reasoning);

        let mut fourth_messages = third_messages.clone();
        fourth_messages.extend([
            json!({ "role": "assistant", "content": "done" }),
            json!({ "role": "user", "content": "continue" }),
        ]);
        let fourth = run_turn(
            &cx,
            &mock,
            &account,
            &with_messages(&base, Value::Array(fourth_messages)),
            json!([]),
            "answer",
        )
        .await;
        let fourth_input = input_items(&fourth);
        assert_eq!(fourth_input[..third_input.len()], third_input[..]);
        let replayed: Vec<Value> =
            fourth_input.iter().filter(|i| i["type"] == json!("reasoning")).cloned().collect();
        assert_eq!(replayed, vec![first_reasoning, second_reasoning]);
        assert_eq!(fourth_input.iter().filter(|i| i["type"] == json!("function_call")).count(), 3);
    }

    #[tokio::test]
    async fn keeps_prior_prefixes_across_tool_only_and_no_reasoning_turns_non_stream() {
        keeps_prior_prefixes(false).await;
    }

    #[tokio::test]
    async fn keeps_prior_prefixes_across_tool_only_and_no_reasoning_turns_stream() {
        keeps_prior_prefixes(true).await;
    }

    #[tokio::test]
    async fn rejects_replay_for_edited_earlier_history_even_when_assistant_text_matches() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let req = replay_req();
        run_turn(&cx, &mock, &account, &req, json!([reasoning_item()]), "answer").await;
        let edited = with_messages(
            &req,
            json!([
                { "role": "user", "content": "changed task" },
                { "role": "assistant", "content": "answer" },
            ]),
        );
        let sent = run_turn(&cx, &mock, &account, &edited, json!([]), "answer").await;
        assert!(!input_items(&sent).contains(&reasoning_item()));
    }

    #[tokio::test]
    async fn does_not_replay_reasoning_across_upstream_accounts() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let req = replay_req();
        run_turn(&cx, &mock, &account, &req, json!([reasoning_item()]), "answer").await;

        let other_row = insert_account(cx.pool(), &account.row.user_id, "codex", &account.credential).await;
        let other = AcquiredAccount { row: other_row, credential: account.credential.clone() };
        let next = with_messages(
            &req,
            json!([{ "role": "user", "content": "hi" }, { "role": "assistant", "content": "answer" }]),
        );
        let sent = run_turn(&cx, &mock, &other, &next, json!([]), "answer").await;
        assert!(!input_items(&sent).contains(&reasoning_item()));
    }

    #[tokio::test]
    async fn does_not_overwrite_history_with_an_unrelated_no_reasoning_completion() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let req = replay_req();
        run_turn(&cx, &mock, &account, &req, json!([reasoning_item()]), "answer").await;
        let before = stored(&cx, &account, "sess_1").await.expect("first turn stored");
        run_turn(&cx, &mock, &account, &req, json!([{ "type": "message", "content": [] }]), "answer").await;
        assert_eq!(stored(&cx, &account, "sess_1").await.as_ref(), Some(&before));
        assert_eq!(
            codex_reasoning_replay_cache_key_for_test("key_1", "gpt-5.6-sol", &format!("sess_1\0{}", account.row.id)),
            codex_reasoning_replay_cache_key_for_test("key_1", "gpt-5.6-sol", &format!("sess_1\0{}", account.row.id))
        );
    }

    // ----- OAuth refresh ---------------------------------------------------

    #[tokio::test]
    async fn refreshes_an_expired_credential_once_and_persists_it() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let expired = StoredCredential {
            access_token: "old-access".into(),
            refresh_token: Some("old-refresh".into()),
            expires_at: Some(iso_from_ms(crate::app::now_ms() - 60_000)),
            ..Default::default()
        };
        let row = insert_account(cx.pool(), &account.row.user_id, "codex", &expired).await;
        let stale = AcquiredAccount { row, credential: expired };
        mock.expect(|req| {
            let body = String::from_utf8_lossy(req.body.as_deref().unwrap_or_default()).into_owned();
            assert!(body.contains("grant_type=refresh_token"));
            assert!(body.contains("refresh_token=old-refresh"));
            assert!(body.contains(&format!("client_id={DEFAULT_CLIENT_ID}")));
            Ok(UpstreamResponse::json(
                StatusCode::OK,
                &json!({ "access_token": "winner-access", "refresh_token": "winner-refresh", "expires_in": 3600 }),
            ))
        });

        let refreshed = CodexAdapter.refresh_if_needed(&cx, stale.clone()).await.unwrap();
        assert_eq!(refreshed.credential.access_token, "winner-access");
        assert_eq!(refreshed.credential.refresh_token.as_deref(), Some("winner-refresh"));
        assert_eq!(mock.requests().len(), 1);
        assert_eq!(mock.requests()[0].url, TOKEN_URL);

        // A later caller still holding the stale credential reads the persisted one without
        // a second token exchange (the mock would panic on an unexpected call).
        let again = CodexAdapter.refresh_if_needed(&cx, stale).await.unwrap();
        assert_eq!(again.credential.access_token, "winner-access");
        assert_eq!(mock.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_credential_without_a_refresh_token_is_never_refreshed() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let out = CodexAdapter.refresh_if_needed(&cx, account.clone()).await.unwrap();
        assert_eq!(out.credential.access_token, "tok_test");
        assert!(mock.requests().is_empty());
    }

    #[test]
    fn the_refresh_guard_follows_the_expiry_window() {
        let with = |refresh: Option<&str>, expires_at: Option<String>| StoredCredential {
            access_token: "a".into(),
            refresh_token: refresh.map(str::to_string),
            expires_at,
            ..Default::default()
        };
        assert!(!codex_needs_refresh(&with(None, None)));
        assert!(!codex_needs_refresh(&with(Some(""), None)));
        // No parseable expiry means refresh now.
        assert!(codex_needs_refresh(&with(Some("r"), None)));
        assert!(codex_needs_refresh(&with(Some("r"), Some("not a date".into()))));
        assert!(codex_needs_refresh(&with(Some("r"), Some(iso_from_ms(crate::app::now_ms() + 30_000)))));
        assert!(!codex_needs_refresh(&with(Some("r"), Some(iso_from_ms(crate::app::now_ms() + 600_000)))));
    }

    // ----- native Responses ingress ---------------------------------------

    fn native(client: Value, reasoning: Option<Value>) -> Map<String, Value> {
        build_codex_native_request_body(client.as_object().unwrap(), "gpt-5.6-sol", reasoning.as_ref())
    }

    #[test]
    fn the_native_body_forces_store_stream_model_and_include() {
        let body = native(json!({ "model": "whatever", "input": [], "store": true }), None);
        assert_eq!(body["model"], json!("gpt-5.6-sol"));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        // An include the client already sent is kept, not duplicated.
        let body = native(json!({ "include": ["reasoning.encrypted_content", 5] }), None);
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }

    #[test]
    fn the_native_body_hoists_system_items_and_shortens_call_ids() {
        let long = "c".repeat(100);
        let body = native(
            json!({
                "instructions": "base",
                "input": [
                    { "type": "message", "role": "system", "content": [{ "text": "one" }] },
                    { "role": "system", "content": "two" },
                    { "type": "function_call", "call_id": long, "name": "f", "arguments": "{}" },
                    { "type": "function_call_output", "call_id": long, "output": "ok" },
                    { "type": "reasoning", "encrypted_content": "echoed" },
                ],
            }),
            None,
        );
        assert_eq!(body["instructions"], json!("base\n\none\n\ntwo"));
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["call_id"], input[1]["call_id"]);
        assert_eq!(input[0]["call_id"].as_str().unwrap().len(), 64);
        // A client's own reasoning item rides through untouched — the replay cache is not
        // consulted on this path.
        assert_eq!(input[2], json!({ "type": "reasoning", "encrypted_content": "echoed" }));

        // A string input becomes one user message, and no instructions means the field goes.
        let body = native(json!({ "input": "hi" }), None);
        assert!(!body.contains_key("instructions"));
        assert_eq!(body["input"], json!([{ "type": "message", "role": "user", "content": "hi" }]));
    }

    #[test]
    fn the_native_body_merges_reasoning_and_drops_a_none_effort() {
        let body = native(
            json!({ "reasoning": { "summary": "concise", "other": 1 } }),
            Some(json!({ "effort": "xhigh", "summary": "auto" })),
        );
        // The client's own summary wins; the clamped effort is ours.
        assert_eq!(body["reasoning"], json!({ "summary": "concise", "other": 1, "effort": "xhigh" }));

        let body = native(json!({ "reasoning": { "effort": "high", "summary": "concise" } }), None);
        assert_eq!(body["reasoning"], json!({ "summary": "concise" }));
        let body = native(json!({ "reasoning": { "effort": "high" } }), None);
        assert!(!body.contains_key("reasoning"));
    }

    #[test]
    fn the_native_body_drops_tools_without_tools_and_fits_the_cache_key() {
        let body = native(json!({ "tools": [], "tool_choice": "auto", "parallel_tool_calls": true }), None);
        assert!(!body.contains_key("tools") && !body.contains_key("tool_choice") && !body.contains_key("parallel_tool_calls"));
        let body = native(json!({ "tools": [{ "type": "web_search" }], "tool_choice": "auto" }), None);
        assert_eq!(body["tools"], json!([{ "type": "web_search" }]));
        assert_eq!(body["tool_choice"], json!("auto"));

        let body = native(json!({ "prompt_cache_key": "z".repeat(200) }), None);
        assert_eq!(body["prompt_cache_key"].as_str().unwrap().len(), 64);
        let body = native(json!({ "prompt_cache_key": "" }), None);
        assert!(!body.contains_key("prompt_cache_key"));

        let mut client = Map::new();
        for field in CODEX_REJECTED_BODY_FIELDS {
            client.insert(field.into(), json!("reject"));
        }
        let body = build_codex_native_request_body(&client, "m", None);
        for field in CODEX_REJECTED_BODY_FIELDS {
            assert!(!body.contains_key(field));
        }
    }

    #[tokio::test]
    async fn the_native_path_streams_the_upstream_sse_untouched_and_never_consults_replay() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        let sse = "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"reasoning\"}]}}\n\n";
        mock.expect(move |_| Ok(UpstreamResponse::sse(StatusCode::OK, sse)));
        let req = ChatCompletionRequest {
            stream: Some(true),
            responses_body: json!({ "input": [{ "role": "user", "content": "hi" }] }).as_object().cloned(),
            ..replay_req()
        };
        let extras = CallExtras { api_key_id: Some("key_1".into()), ..Default::default() };
        let response = CodexAdapter.chat_completions(&cx, &account, &req, &extras).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[http::header::CONTENT_TYPE], "text/event-stream; charset=utf-8");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&body), sse);
        assert!(stored(&cx, &account, "sess_1").await.is_none());
    }

    #[tokio::test]
    async fn the_native_non_stream_path_returns_the_terminal_response_object() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        mock.expect(|_| {
            Ok(UpstreamResponse::sse(
                StatusCode::OK,
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"output\":[]}}\n\n",
            ))
        });
        let req = ChatCompletionRequest {
            responses_body: json!({ "input": [] }).as_object().cloned(),
            ..replay_req()
        };
        let response = CodexAdapter.chat_completions(&cx, &account, &req, &CallExtras::default()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!({ "id": "resp_1", "output": [] }));
    }

    // ----- upstream failures ----------------------------------------------

    #[tokio::test]
    async fn an_upstream_413_is_surfaced_as_is_with_its_own_body() {
        // The relay used to synthesize this envelope; direct egress simply passes the
        // upstream's own 413 through, and dispatch classifies it (docs/api.md § Errors).
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        for stream in [false, true] {
            mock.expect(|_| {
                Ok(UpstreamResponse::json(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &json!({ "error": { "message": "too large", "code": "request_too_large" } }),
                ))
            });
            let req = ChatCompletionRequest { stream: Some(stream), ..replay_req() };
            let response = CodexAdapter.chat_completions(&cx, &account, &req, &CallExtras::default()).await.unwrap();
            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({ "error": { "message": "too large", "code": "request_too_large" } })
            );
        }
    }

    #[tokio::test]
    async fn a_mid_turn_failure_becomes_502_not_a_fabricated_completion() {
        let mock = MockTransport::new();
        let Some((cx, account)) = codex_fixture(&mock).await else { return skip_without_db() };
        mock.expect(|_| {
            Ok(UpstreamResponse::sse(
                StatusCode::OK,
                "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\"}}}\n\n",
            ))
        });
        let response = CodexAdapter
            .chat_completions(&cx, &account, &replay_req(), &CallExtras::default())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({ "error": { "message": "boom", "type": "upstream_error" } })
        );
    }

    #[test]
    fn the_user_agent_tracks_the_single_client_version_constant() {
        assert!(CODEX_USER_AGENT.contains(CODEX_CLIENT_VERSION));
        assert!(CODEX_USER_AGENT.starts_with("codex-tui/"));
    }
}
