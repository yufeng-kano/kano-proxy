//! The Claude Code provider (docs/providers.md § Claude Code,
//! docs/auth.md § Claude Code OAuth).
//!
//! Two surfaces share one upstream: the native `/anthropic` passthrough (auth injection,
//! faithful beta merge, fixed system prepend) and the `/openai/v1` conversion path, which
//! authors the Anthropic body itself through `proxy::openai_anthropic`. Every call goes
//! through `cx.transport()`; SSE bodies are piped, never buffered.

use async_trait::async_trait;
use axum::body::Body;
use axum::response::Response;
use http::{header, HeaderMap, StatusCode};
use serde_json::{json, Map, Value};

use crate::pool::{AcquiredAccount, StoredCredential};
use crate::providers::refresh::refresh_oauth_credential;
use crate::providers::types::{
    AdapterError, CallExtras, ChatCompletionRequest, DynAdapter, FetchedUsage, ListedModels, ProviderAdapter, UpstreamModel,
    UsageWindow,
};
use crate::providers::ProviderId;
use crate::proxy::openai_anthropic::{
    add_conversion_cache_control, anthropic_sse_to_openai_stream, anthropic_to_openai_response, move_retired_output_format,
    openai_to_anthropic_messages, AnthropicMessagesInput,
};
use crate::upstream::{UpstreamRequest, UpstreamResponse};
use crate::utils::reasoning::map_reasoning;
use crate::AppState;

const ANTHROPIC_API: &str = "https://api.anthropic.com";
const OAUTH_TOKEN: &str = "https://console.anthropic.com/v1/oauth/token";
const REQUIRED_SYSTEM: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

const DEFAULT_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Claude Code CLI client fingerprint. The OAuth upstream expects requests to look like the
/// CLI that owns these tokens; a real client's own values are forwarded when it sends them —
/// these are only the fallback for surfaces with no client headers to relay.
pub const CLAUDE_CLIENT_FINGERPRINT: [(&str, &str); 5] = [
    ("user-agent", "claude-cli/2.1.280 (external, cli)"),
    ("x-stainless-package-version", "0.112.1"),
    ("x-stainless-runtime-version", "v26.3.0"),
    ("x-stainless-os", "MacOS"),
    ("x-stainless-arch", "arm64"),
];

/// The fingerprint value for `name`, preferring whatever the client already sent so a genuine
/// Claude Code client keeps its own identity end to end.
fn fingerprint_value(headers: Option<&HeaderMap>, name: &str, fallback: &str) -> String {
    headers
        .and_then(|h| h.get(name))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// Adds every fingerprint field to `req` (client value first, baseline otherwise).
fn with_client_fingerprint(mut req: UpstreamRequest, headers: Option<&HeaderMap>) -> UpstreamRequest {
    for (name, fallback) in CLAUDE_CLIENT_FINGERPRINT {
        req = req.header(name, &fingerprint_value(headers, name, fallback));
    }
    req
}

fn client_id(cx: &AppState) -> String {
    cx.config().claude_code_oauth_client_id.clone().unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string())
}

/// `refreshOAuthCredential` with the Claude expiry rule: refresh within 60s of expiry, or when
/// the stored expiry is missing/unparseable, and only when there is a refresh token at all.
async fn refresh_claude(cx: &AppState, account: AcquiredAccount) -> AcquiredAccount {
    let now = crate::app::now_ms();
    let fallback_client_id = client_id(cx);
    refresh_oauth_credential(
        cx,
        account,
        move |credential: &StoredCredential| {
            if credential.refresh_token.is_none() {
                return false;
            }
            let exp = credential
                .expires_at
                .as_deref()
                .and_then(crate::db::accounts::parse_iso_ms)
                .unwrap_or(0);
            exp == 0 || exp - 60_000 <= now
        },
        move |credential: &StoredCredential| {
            let credential = credential.clone();
            let client_id = credential.client_id.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| fallback_client_id.clone());
            async move {
                let body = json!({
                    "grant_type": "refresh_token",
                    "refresh_token": credential.refresh_token,
                    "client_id": client_id,
                });
                let res = cx.transport().send(UpstreamRequest::post(OAUTH_TOKEN).json(&body)).await.ok()?;
                if !res.status.is_success() {
                    return None;
                }
                let json = res.json_value().await.ok()?;
                let access_token = json.get("access_token")?.as_str()?.to_string();
                let refresh_token = json
                    .get("refresh_token")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| credential.refresh_token.clone());
                let expires_at = match json.get("expires_in").and_then(Value::as_f64) {
                    Some(seconds) => Some(crate::db::accounts::iso_from_ms(crate::app::now_ms() + (seconds * 1000.0) as i64)),
                    None => credential.expires_at.clone(),
                };
                Some(StoredCredential { access_token, refresh_token, expires_at, ..credential })
            }
        },
    )
    .await
}

const EFFORT_BETA: &str = "effort-2025-11-24";

/// The only two betas the Claude Code OAuth upstream requires to accept a request at all.
/// Always first, in this order, on every native-passthrough and conversion-path request.
const REQUIRED_BETAS: [&str; 2] = ["oauth-2025-04-20", "claude-code-20250219"];

/// Feature betas the `/openai/v1` conversion path opts into unconditionally, since there the
/// proxy authors the upstream Anthropic request itself and has no client beta header to be
/// faithful to. NOT used by the native passthrough — see [`resolve_beta_header`].
const CONVERSION_BETAS: [&str; 2] = ["interleaved-thinking-2025-05-14", "fine-grained-tool-streaming-2025-05-14"];

/// Builds an `anthropic-beta` header value: `REQUIRED_BETAS` first, then each comma-separated
/// entry of `extra` in order, deduped against the required pair and against earlier entries in
/// `extra` itself.
pub fn beta_headers(extra: Option<&str>) -> String {
    let mut base: Vec<&str> = REQUIRED_BETAS.to_vec();
    if let Some(extra) = extra {
        for part in extra.split(',') {
            let t = part.trim();
            if !t.is_empty() && !base.contains(&t) {
                base.push(t);
            }
        }
    }
    base.join(",")
}

/// `anthropic-beta` for the native `/anthropic` passthrough — faithful, not opinionated: the
/// two OAuth-required betas, then the client's own list verbatim (deduped, order preserved).
/// Feature betas are never force-added here; `effort-2025-11-24` is the one exception, added
/// automatically when the (patched) body carries `output_config`, deduped the same way.
pub fn resolve_beta_header(client_beta: Option<&str>, has_output_config: bool) -> String {
    let extras: Vec<&str> = [client_beta, has_output_config.then_some(EFFORT_BETA)].into_iter().flatten().collect();
    if extras.is_empty() {
        beta_headers(None)
    } else {
        beta_headers(Some(&extras.join(",")))
    }
}

/// Idempotent fixed system prepend (docs/providers.md § Claude Code).
fn prepend_required_system(body: &Map<String, Value>) -> Map<String, Value> {
    let required = json!({ "type": "text", "text": REQUIRED_SYSTEM });
    match body.get("system") {
        Some(Value::String(sys)) => {
            if sys.starts_with(REQUIRED_SYSTEM) {
                return body.clone();
            }
            let mut out = body.clone();
            out.insert("system".into(), json!([required, { "type": "text", "text": sys }]));
            out
        }
        Some(Value::Array(sys)) => {
            let first = sys.first();
            let is_prepended = first.and_then(|b| b.get("type")).and_then(Value::as_str) == Some("text")
                && first.and_then(|b| b.get("text")).and_then(Value::as_str) == Some(REQUIRED_SYSTEM);
            if is_prepended {
                return body.clone();
            }
            let mut blocks = vec![required];
            blocks.extend(sys.iter().cloned());
            let mut out = body.clone();
            out.insert("system".into(), Value::Array(blocks));
            out
        }
        _ => {
            let mut out = body.clone();
            out.insert("system".into(), json!([required]));
            out
        }
    }
}

/// The upstream response as the client sees it on a passthrough surface: status and headers
/// kept, body piped (framing headers are recomputed by the server).
fn passthrough(res: UpstreamResponse) -> Response {
    let mut builder = Response::builder().status(res.status);
    for (name, value) in res.headers.iter() {
        if name == header::CONTENT_LENGTH || name == header::TRANSFER_ENCODING {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder.body(Body::from_stream(res.body)).expect("response builds")
}

fn json_response(status: StatusCode, value: &Value) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(value).expect("json serializes")))
        .expect("response builds")
}

/// Shared native-passthrough forward for `/v1/messages` and `/v1/messages/count_tokens` —
/// same auth injection, beta header resolution and required-system prepend either needs.
async fn forward_to_anthropic(
    url: &str,
    cx: &AppState,
    account: &AcquiredAccount,
    body: &Value,
    headers: &HeaderMap,
    extras: &CallExtras,
) -> Result<Response, AdapterError> {
    let acc = refresh_claude(cx, account.clone()).await;
    let raw = body.as_object().cloned().unwrap_or_default();
    let prepended = prepend_required_system(&raw);
    let patched = move_retired_output_format(&prepended).into_owned();
    let client_beta = headers.get("anthropic-beta").and_then(|v| v.to_str().ok());
    let has_output_config = patched.contains_key("output_config");
    let anthropic_version = headers.get("anthropic-version").and_then(|v| v.to_str().ok()).unwrap_or("2023-06-01");

    let mut req = UpstreamRequest::post(url)
        .json(&Value::Object(patched))
        .header("authorization", &format!("Bearer {}", acc.credential.access_token))
        .header("anthropic-version", anthropic_version)
        .header("anthropic-beta", &resolve_beta_header(client_beta, has_output_config));
    req = with_client_fingerprint(req, Some(headers));
    if let Some(timeout) = extras.first_byte_timeout {
        req = req.timeout(timeout);
    }
    Ok(passthrough(cx.transport().send(req).await?))
}

/// Model-id prefix of the Fable family — the only Claude Code models gated per seat
/// (docs/providers.md § Claude Code "Fable seat eligibility").
pub const FABLE_MODEL_PREFIX: &str = "claude-fable";

pub struct ClaudeCodeAdapter;

/// The builtin claude-code adapter (`providers::registry` wires it in).
pub fn adapter() -> DynAdapter {
    std::sync::Arc::new(ClaudeCodeAdapter)
}

#[async_trait]
impl ProviderAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &str {
        ProviderId::ClaudeCode.as_str()
    }

    async fn refresh_if_needed(&self, cx: &AppState, account: AcquiredAccount) -> Result<AcquiredAccount, AdapterError> {
        Ok(refresh_claude(cx, account).await)
    }

    // A standard Team seat has no Fable: Anthropic answers with a bare 429 that the pool would
    // otherwise treat as a rate limit and bench the whole account for 300s. Team Premium
    // reports the Max 5x tier string — the same test Claude Code CLI 2.1.259's
    // isTeamPremiumSubscriber runs. Every other case, including a missing profile, fails open.
    fn supports_model(&self, meta: Option<&Map<String, Value>>, upstream_model: &str) -> bool {
        if !upstream_model.starts_with(FABLE_MODEL_PREFIX) {
            return true;
        }
        if meta.and_then(|m| m.get("plan_type")).and_then(Value::as_str) != Some("claude_team") {
            return true;
        }
        match meta.and_then(|m| m.get("rate_limit_tier")) {
            Some(Value::String(tier)) => tier == "default_claude_max_5x",
            _ => true,
        }
    }

    fn has_list_models(&self) -> bool {
        true
    }

    async fn list_models(&self, cx: &AppState, account: &AcquiredAccount) -> ListedModels {
        let acc = refresh_claude(cx, account.clone()).await;
        let req = with_client_fingerprint(
            UpstreamRequest::get(format!("{ANTHROPIC_API}/v1/models?limit=100"))
                .header("authorization", &format!("Bearer {}", acc.credential.access_token))
                .header("anthropic-version", "2023-06-01")
                .header("anthropic-beta", "oauth-2025-04-20"),
            None,
        );
        let res = match cx.transport().send(req).await {
            Ok(res) => res,
            Err(e) => return ListedModels { models: Vec::new(), error: Some(e.to_string()) },
        };
        if !res.status.is_success() {
            return ListedModels { models: Vec::new(), error: Some(format!("models {}", res.status.as_u16())) };
        }
        let json = match res.json_value().await {
            Ok(json) => json,
            Err(e) => return ListedModels { models: Vec::new(), error: Some(e.to_string()) },
        };
        let models = json
            .get("data")
            .and_then(Value::as_array)
            .map(|data| {
                data.iter()
                    .filter_map(|m| {
                        let id = m.get("id").and_then(Value::as_str).filter(|id| !id.is_empty())?;
                        Some(UpstreamModel {
                            id: id.to_string(),
                            display_name: m.get("display_name").and_then(Value::as_str).map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        ListedModels { models, error: None }
    }

    fn has_messages(&self) -> bool {
        true
    }

    async fn messages(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        body: &Value,
        headers: &HeaderMap,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        forward_to_anthropic(&format!("{ANTHROPIC_API}/v1/messages"), cx, account, body, headers, extras).await
    }

    fn has_count_tokens(&self) -> bool {
        true
    }

    async fn count_tokens(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        body: &Value,
        headers: &HeaderMap,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        forward_to_anthropic(&format!("{ANTHROPIC_API}/v1/messages/count_tokens"), cx, account, body, headers, extras).await
    }

    async fn chat_completions(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        req: &ChatCompletionRequest,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let acc = refresh_claude(cx, account.clone()).await;
        let mapped = map_reasoning(ProviderId::ClaudeCode, req.reasoning_effort);
        let anthropic_body = openai_to_anthropic_messages(&AnthropicMessagesInput {
            model: req.upstream_model.clone(),
            messages: req.messages.clone(),
            max_tokens: req.max_tokens.unwrap_or(4096),
            stream: req.stream,
            tools: req.tools.clone(),
            tool_choice: req.tool_choice.clone(),
            response_format: req.response_format.clone(),
            thinking: mapped.get("thinking").cloned(),
            output_config: mapped.get("output_config").cloned(),
            stop: req.stop.clone(),
            temperature: req.temperature,
            top_p: req.top_p,
        });
        // Proxy-placed cache breakpoints (docs/api.md § Prompt cache), applied after the fixed
        // prepend so the system marker lands on the last block.
        let with_system = add_conversion_cache_control(
            &prepend_required_system(&anthropic_body),
            req.prompt_cache_key.as_deref().is_some_and(|k| !k.is_empty()),
        );
        let betas = [CONVERSION_BETAS[0], CONVERSION_BETAS[1], EFFORT_BETA].join(",");
        let mut upstream = UpstreamRequest::post(format!("{ANTHROPIC_API}/v1/messages"))
            .json(&Value::Object(with_system))
            .header("authorization", &format!("Bearer {}", acc.credential.access_token))
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", &beta_headers(Some(&betas)));
        // No client headers to relay on this surface — the proxy authors the whole upstream
        // request, so the fallback fingerprint is all there is.
        upstream = with_client_fingerprint(upstream, None);
        if let Some(timeout) = extras.first_byte_timeout {
            upstream = upstream.timeout(timeout);
        }
        let res = cx.transport().send(upstream).await?;

        if req.stream == Some(true) {
            if !res.status.is_success() {
                return Ok(passthrough(res));
            }
            // Same model id as the non-stream path: client-facing provider/model.
            let status = res.status;
            let body = anthropic_sse_to_openai_stream(res.body, &req.raw_model);
            return Ok(Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
                .header(header::CACHE_CONTROL, "no-cache")
                .body(Body::from_stream(body))
                .expect("response builds"));
        }

        let status = res.status;
        let content_type = res.content_type().unwrap_or("application/json").to_string();
        let text = res.text().await.map_err(|e| AdapterError::Other(e.into()))?;
        if !status.is_success() {
            return Ok(Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(text))
                .expect("response builds"));
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(message)) => {
                let openai = anthropic_to_openai_response(&message, &req.raw_model);
                Ok(json_response(StatusCode::OK, &Value::Object(openai)))
            }
            _ => Ok(Response::builder().status(status).body(Body::from(text)).expect("response builds")),
        }
    }

    fn has_fetch_usage(&self) -> bool {
        true
    }

    async fn fetch_usage(&self, cx: &AppState, account: &AcquiredAccount) -> FetchedUsage {
        let acc = refresh_claude(cx, account.clone()).await;
        let authorized = |url: String| {
            with_client_fingerprint(
                UpstreamRequest::get(url)
                    .header("authorization", &format!("Bearer {}", acc.credential.access_token))
                    .header("anthropic-beta", "oauth-2025-04-20")
                    .header("anthropic-version", "2023-06-01"),
                None,
            )
        };
        let (usage_res, profile_res) = futures::future::join(
            cx.transport().send(authorized(format!("{ANTHROPIC_API}/api/oauth/usage"))),
            cx.transport().send(authorized(format!("{ANTHROPIC_API}/api/oauth/profile"))),
        )
        .await;

        let usage_res = match usage_res {
            Ok(res) => res,
            Err(e) => {
                return FetchedUsage { windows: Vec::new(), account: Map::new(), stale: true, error: Some(e.to_string()), edge_blocked: false }
            }
        };
        if !usage_res.status.is_success() {
            return FetchedUsage {
                windows: Vec::new(),
                account: Map::new(),
                stale: true,
                error: Some(format!("usage {}", usage_res.status.as_u16())),
                edge_blocked: false,
            };
        }
        let usage = usage_res.json_value().await.unwrap_or(Value::Null);
        let profile = match profile_res {
            Ok(res) if res.status.is_success() => res.json_value().await.unwrap_or(Value::Null),
            _ => Value::Null,
        };

        let mut windows: Vec<UsageWindow> = Vec::new();
        for (key, label) in [("five_hour", "5h"), ("seven_day", "Week")] {
            let Some(window) = usage.get(key).filter(|v| !v.is_null()) else { continue };
            windows.push(UsageWindow {
                label: label.to_string(),
                utilization: window.get("utilization").and_then(Value::as_f64),
                resets_at: window.get("resets_at").and_then(Value::as_str).map(str::to_string),
                value: None,
            });
        }
        if let Some(limits) = usage.get("limits").and_then(Value::as_array) {
            for lim in limits {
                if lim.get("kind").and_then(Value::as_str) != Some("weekly_scoped") {
                    continue;
                }
                // Entries in `limits[]` carry the percent as `percent`, while the top-level
                // `five_hour`/`seven_day` objects call the same number `utilization`.
                let pct = lim.get("percent").and_then(Value::as_f64).or_else(|| lim.get("utilization").and_then(Value::as_f64));
                let label = lim
                    .get("scope")
                    .and_then(|s| s.get("model"))
                    .and_then(|m| m.get("display_name"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("scoped");
                windows.push(UsageWindow {
                    label: label.to_string(),
                    utilization: pct,
                    resets_at: lim.get("resets_at").and_then(Value::as_str).map(str::to_string),
                    value: None,
                });
            }
        }

        // Only keys with a value: this object is spread over the stored `account_meta_json`,
        // and an explicit undefined would erase a plan fact the seat rule (supports_model)
        // reads whenever the profile call failed or came back without an organization.
        let email = profile
            .get("account")
            .and_then(|a| a.get("email"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| acc.credential.email.clone());
        let mut profile_account = Map::new();
        profile_account.insert("email".into(), email.map(Value::String).unwrap_or(Value::Null));
        let org = profile.get("organization");
        if let Some(plan) = org.and_then(|o| o.get("organization_type")).and_then(Value::as_str).filter(|s| !s.is_empty()) {
            profile_account.insert("plan_type".into(), json!(plan));
        }
        if let Some(tier) = org.and_then(|o| o.get("rate_limit_tier")).and_then(Value::as_str).filter(|s| !s.is_empty()) {
            profile_account.insert("rate_limit_tier".into(), json!(tier));
        }
        FetchedUsage { windows, account: profile_account, stale: false, error: None, edge_blocked: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_account, insert_user, skip_without_db, test_pool, test_state};
    use crate::upstream::MockTransport;
    use http::HeaderValue;
    use std::sync::Arc;

    const BASE: [&str; 2] = ["oauth-2025-04-20", "claude-code-20250219"];
    const CONVERSION_EXTRA: [&str; 2] = ["interleaved-thinking-2025-05-14", "fine-grained-tool-streaming-2025-05-14"];

    fn split(header: &str) -> Vec<&str> {
        header.split(',').collect()
    }

    // ---------------------------------------------------------------- betaHeaders

    #[test]
    fn beta_headers_returns_the_two_required_flags_with_no_extra() {
        assert_eq!(split(&beta_headers(None)), BASE);
        assert_eq!(split(&beta_headers(Some(""))), BASE);
    }

    #[test]
    fn beta_headers_appends_a_client_extra_not_already_in_the_base_set() {
        assert_eq!(split(&beta_headers(Some("context-management-2025-06-27"))), ["oauth-2025-04-20", "claude-code-20250219", "context-management-2025-06-27"]);
    }

    #[test]
    fn beta_headers_does_not_double_an_extra_that_duplicates_a_base_flag() {
        assert_eq!(split(&beta_headers(Some("oauth-2025-04-20"))), BASE);
    }

    #[test]
    fn beta_headers_dedups_repeated_extras_within_the_same_string() {
        assert_eq!(split(&beta_headers(Some("foo,foo,bar"))), ["oauth-2025-04-20", "claude-code-20250219", "foo", "bar"]);
    }

    #[test]
    fn beta_headers_preserves_extra_order_verbatim_including_feature_betas() {
        assert_eq!(
            split(&beta_headers(Some(&CONVERSION_EXTRA.join(",")))),
            [BASE.as_slice(), CONVERSION_EXTRA.as_slice()].concat()
        );
    }

    // ------------------------------------------- resolveBetaHeader (native passthrough)

    #[test]
    fn no_client_betas_and_no_output_config_emits_exactly_the_required_pair() {
        let header = resolve_beta_header(None, false);
        assert_eq!(split(&header), BASE);
        assert!(!split(&header).contains(&"interleaved-thinking-2025-05-14"));
        assert!(!split(&header).contains(&"fine-grained-tool-streaming-2025-05-14"));
    }

    #[test]
    fn adds_the_effort_beta_when_the_patched_body_carries_output_config() {
        assert_eq!(split(&resolve_beta_header(None, true)), [BASE.as_slice(), &[EFFORT_BETA]].concat());
    }

    #[test]
    fn does_not_add_the_effort_beta_without_output_config_even_with_other_client_betas() {
        let header = resolve_beta_header(Some("context-management-2025-06-27"), false);
        assert_eq!(split(&header), [BASE.as_slice(), &["context-management-2025-06-27"]].concat());
        assert!(!header.contains(EFFORT_BETA));
    }

    #[test]
    fn does_not_double_a_client_supplied_effort_beta_when_output_config_is_present() {
        let header = resolve_beta_header(Some(EFFORT_BETA), true);
        assert_eq!(split(&header).iter().filter(|b| **b == EFFORT_BETA).count(), 1);
        assert_eq!(split(&header), [BASE.as_slice(), &[EFFORT_BETA]].concat());
    }

    #[test]
    fn keeps_other_client_betas_alongside_the_auto_added_effort_beta() {
        let header = resolve_beta_header(Some("context-management-2025-06-27"), true);
        assert_eq!(split(&header), [BASE.as_slice(), &["context-management-2025-06-27", EFFORT_BETA]].concat());
    }

    #[test]
    fn treats_an_absent_client_beta_the_same_as_none() {
        assert_eq!(split(&resolve_beta_header(None, true)), [BASE.as_slice(), &[EFFORT_BETA]].concat());
    }

    #[test]
    fn honors_the_clients_own_choice_to_send_interleaved_thinking() {
        let header = resolve_beta_header(Some("interleaved-thinking-2025-05-14"), false);
        assert_eq!(split(&header), [BASE.as_slice(), &["interleaved-thinking-2025-05-14"]].concat());
    }

    #[test]
    fn preserves_the_clients_full_beta_list_verbatim_and_in_order() {
        let client_list = ["fine-grained-tool-streaming-2025-05-14", "context-management-2025-06-27", "interleaved-thinking-2025-05-14"];
        let header = resolve_beta_header(Some(&client_list.join(",")), false);
        assert_eq!(split(&header), [BASE.as_slice(), client_list.as_slice()].concat());
    }

    #[test]
    fn a_client_sending_one_of_the_fixed_pair_is_not_doubled() {
        let header = resolve_beta_header(Some("claude-code-20250219,context-management-2025-06-27"), false);
        assert_eq!(split(&header), [BASE.as_slice(), &["context-management-2025-06-27"]].concat());
    }

    // ------------------------------------------------------------------ supportsModel

    #[test]
    fn supports_model_fable_seat_eligibility() {
        let adapter = ClaudeCodeAdapter;
        let meta = |v: Value| v.as_object().cloned().unwrap();
        let team_standard = meta(json!({ "plan_type": "claude_team", "rate_limit_tier": "default_raven" }));
        // A standard Team seat cannot serve a claude-fable model.
        assert!(!adapter.supports_model(Some(&team_standard), "claude-fable-5-1"));
        assert!(!adapter.supports_model(Some(&team_standard), "claude-fable-5-1[1m]"));
        // The same seat serves every non-Fable model.
        assert!(adapter.supports_model(Some(&team_standard), "claude-sonnet-5"));
        assert!(adapter.supports_model(Some(&team_standard), "claude-opus-5"));
        // Team Premium reports the Max 5x tier string and stays eligible.
        let premium = meta(json!({ "plan_type": "claude_team", "rate_limit_tier": "default_claude_max_5x" }));
        assert!(adapter.supports_model(Some(&premium), "claude-fable-5-1"));
        // Max plans are eligible regardless of tier.
        for tier in ["default_claude_max_5x", "default_claude_max_20x"] {
            let max = meta(json!({ "plan_type": "claude_max", "rate_limit_tier": tier }));
            assert!(adapter.supports_model(Some(&max), "claude-fable-5-1"));
        }
        // Fails open on missing facts.
        assert!(adapter.supports_model(None, "claude-fable-5-1"));
        assert!(adapter.supports_model(Some(&Map::new()), "claude-fable-5-1"));
        assert!(adapter.supports_model(Some(&meta(json!({ "email": "x@example.com" }))), "claude-fable-5-1"));
        assert!(adapter.supports_model(Some(&meta(json!({ "plan_type": "claude_team" }))), "claude-fable-5-1"));
        assert!(adapter.supports_model(Some(&meta(json!({ "plan_type": "claude_team", "rate_limit_tier": 42 }))), "claude-fable-5-1"));
    }

    // ----------------------------------------------------------------- adapter calls

    /// An account whose credential never needs refreshing (no refresh token), so the adapter
    /// tests exercise exactly the upstream call under test.
    async fn account(pool: &sqlx::PgPool) -> AcquiredAccount {
        let user = insert_user(pool, &format!("cc-{}@example.com", crate::ids::new_id("t"))).await;
        let credential = StoredCredential { access_token: "tok_test".into(), ..Default::default() };
        let row = insert_account(pool, &user.id, "claude-code", &credential).await;
        AcquiredAccount { row, credential }
    }

    fn message_json() -> Value {
        json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "text", "text": "hi" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 1, "output_tokens": 1 },
        })
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(http::HeaderName::from_bytes(name.as_bytes()).unwrap(), HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn chat_request(messages: Value, tools: Option<Value>, prompt_cache_key: Option<&str>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "claude-code/claude-opus-5".into(),
            raw_model: "claude-code/claude-opus-5".into(),
            upstream_model: "claude-opus-5".into(),
            messages: messages.as_array().cloned().unwrap(),
            tools,
            prompt_cache_key: prompt_cache_key.map(str::to_string),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn chat_completions_places_the_proxy_cache_breakpoints_after_the_fixed_system_prepend() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, message_json());
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;

        let req = chat_request(
            json!([
                { "role": "system", "content": "instructions" },
                { "role": "user", "content": "q1" },
                { "role": "assistant", "content": "a1" },
                { "role": "user", "content": "q2" },
            ]),
            Some(json!([{ "type": "function", "function": { "name": "f", "parameters": { "type": "object", "properties": {} } } }])),
            Some("thread-1"),
        );
        ClaudeCodeAdapter.chat_completions(&cx, &account, &req, &CallExtras::default()).await.unwrap();

        let sent = transport.requests()[0].json();
        let system = sent["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);
        // The prepend is block 0 and carries no marker; the client's own system text is the
        // marked last block.
        assert!(system[0].get("cache_control").is_none());
        assert_eq!(system[1]["text"], json!("instructions"));
        assert_eq!(system[1]["cache_control"], json!({ "type": "ephemeral", "ttl": "1h" }));
        assert_eq!(sent["tools"][0]["cache_control"], json!({ "type": "ephemeral", "ttl": "1h" }));
        let messages = sent["messages"].as_array().unwrap();
        assert_eq!(messages[2]["content"], json!([{ "type": "text", "text": "q2", "cache_control": { "type": "ephemeral", "ttl": "1h" } }]));
        assert_eq!(messages[0]["content"], json!([{ "type": "text", "text": "q1", "cache_control": { "type": "ephemeral", "ttl": "1h" } }]));
        assert!(!serde_json::to_string(&messages[1]).unwrap().contains("cache_control"));
        assert_eq!(serde_json::to_string(&sent).unwrap().matches("\"cache_control\"").count(), 4);
    }

    #[tokio::test]
    async fn chat_completions_sends_the_exact_unchanged_anthropic_beta_header() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, message_json());
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;

        let req = chat_request(json!([{ "role": "user", "content": "hi" }]), None, None);
        let res = ClaudeCodeAdapter.chat_completions(&cx, &account, &req, &CallExtras::default()).await.unwrap();

        let recorded = transport.requests();
        assert_eq!(recorded[0].url, "https://api.anthropic.com/v1/messages");
        assert_eq!(
            recorded[0].header("anthropic-beta"),
            Some("oauth-2025-04-20,claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14,effort-2025-11-24")
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn count_tokens_forwards_with_the_same_header_construction_as_messages() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "input_tokens": 42 }));
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;

        let res = ClaudeCodeAdapter
            .count_tokens(
                &cx,
                &account,
                &json!({ "model": "claude-opus-5", "messages": [{ "role": "user", "content": "hi" }] }),
                &headers(&[("anthropic-beta", "context-management-2025-06-27")]),
                &CallExtras::default(),
            )
            .await
            .unwrap();

        let recorded = transport.requests();
        assert_eq!(recorded[0].url, "https://api.anthropic.com/v1/messages/count_tokens");
        assert_eq!(recorded[0].method, http::Method::POST);
        assert_eq!(recorded[0].header("authorization"), Some("Bearer tok_test"));
        assert_eq!(recorded[0].header("anthropic-version"), Some("2023-06-01"));
        assert_eq!(recorded[0].header("anthropic-beta"), Some("oauth-2025-04-20,claude-code-20250219,context-management-2025-06-27"));
        // Idempotent required-system prepend, same as the messages() adapter path.
        let sent = recorded[0].json();
        assert_eq!(sent["system"], json!([{ "type": "text", "text": REQUIRED_SYSTEM }]));
        assert_eq!(sent["model"], json!("claude-opus-5"));
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn count_tokens_adds_the_effort_beta_when_the_body_carries_output_config() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "input_tokens": 1 }));
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;

        ClaudeCodeAdapter
            .count_tokens(
                &cx,
                &account,
                &json!({ "model": "claude-opus-5", "messages": [], "output_config": { "effort": "high" } }),
                &HeaderMap::new(),
                &CallExtras::default(),
            )
            .await
            .unwrap();

        assert_eq!(transport.requests()[0].header("anthropic-beta"), Some("oauth-2025-04-20,claude-code-20250219,effort-2025-11-24"));
    }

    #[tokio::test]
    async fn count_tokens_never_streams_and_returns_the_upstream_error_as_is() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::BAD_REQUEST, json!({ "error": { "message": "bad request" } }));
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;

        let res = ClaudeCodeAdapter
            .count_tokens(&cx, &account, &json!({ "model": "claude-opus-5", "messages": [] }), &HeaderMap::new(), &CallExtras::default())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!({ "error": { "message": "bad request" } }));
    }

    #[tokio::test]
    async fn messages_moves_a_clients_top_level_output_format_to_output_config_format() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, message_json());
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;

        ClaudeCodeAdapter
            .messages(
                &cx,
                &account,
                &json!({
                    "model": "claude-opus-5",
                    "max_tokens": 10,
                    "messages": [{ "role": "user", "content": "hi" }],
                    "output_format": { "type": "json_schema", "schema": { "type": "object" } },
                }),
                &HeaderMap::new(),
                &CallExtras::default(),
            )
            .await
            .unwrap();

        let sent = transport.requests()[0].json();
        assert!(sent.get("output_format").is_none());
        assert_eq!(sent["output_config"], json!({ "format": { "type": "json_schema", "schema": { "type": "object" } } }));
    }

    // ------------------------------------------------------------ client fingerprint

    #[tokio::test]
    async fn sends_the_cli_fingerprint_on_the_native_passthrough() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;

        ClaudeCodeAdapter
            .messages(&cx, &account, &json!({ "model": "m", "messages": [] }), &HeaderMap::new(), &CallExtras::default())
            .await
            .unwrap();

        let recorded = transport.requests();
        assert_eq!(recorded[0].header("user-agent"), Some(CLAUDE_CLIENT_FINGERPRINT[0].1));
        assert_eq!(recorded[0].header("x-stainless-os"), Some("MacOS"));
        assert_eq!(recorded[0].header("x-stainless-arch"), Some("arm64"));
    }

    #[tokio::test]
    async fn prefers_the_real_clients_own_fingerprint_when_it_sent_one() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;

        ClaudeCodeAdapter
            .messages(
                &cx,
                &account,
                &json!({ "model": "m", "messages": [] }),
                &headers(&[("user-agent", "claude-cli/9.9.9 (external, vscode)"), ("x-stainless-os", "Linux")]),
                &CallExtras::default(),
            )
            .await
            .unwrap();

        let recorded = transport.requests();
        assert_eq!(recorded[0].header("user-agent"), Some("claude-cli/9.9.9 (external, vscode)"));
        assert_eq!(recorded[0].header("x-stainless-os"), Some("Linux"));
        // Unsent fields still fall back to the baseline.
        assert_eq!(recorded[0].header("x-stainless-arch"), Some("arm64"));
    }

    #[tokio::test]
    async fn sends_the_fallback_fingerprint_on_the_conversion_surface() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;

        let req = chat_request(json!([]), None, None);
        ClaudeCodeAdapter.chat_completions(&cx, &account, &req, &CallExtras::default()).await.unwrap();
        assert_eq!(transport.requests()[0].header("user-agent"), Some(CLAUDE_CLIENT_FINGERPRINT[0].1));
    }

    // ------------------------------------------------------------------- fetchUsage

    /// Answers the two concurrent GETs fetch_usage makes, by URL rather than by call order.
    fn stub_usage(transport: &Arc<MockTransport>, usage: Value, profile: Value, usage_status: StatusCode, profile_status: StatusCode) {
        for _ in 0..2 {
            let usage = usage.clone();
            let profile = profile.clone();
            transport.expect(move |req| {
                if req.url.contains("/api/oauth/usage") {
                    Ok(UpstreamResponse::json(usage_status, &usage))
                } else if req.url.contains("/api/oauth/profile") {
                    Ok(UpstreamResponse::json(profile_status, &profile))
                } else {
                    panic!("unexpected upstream request {}", req.url)
                }
            });
        }
    }

    async fn usage_for(pool: &sqlx::PgPool, usage: Value, profile: Value) -> FetchedUsage {
        let transport = MockTransport::new();
        stub_usage(&transport, usage, profile, StatusCode::OK, StatusCode::OK);
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(pool).await;
        ClaudeCodeAdapter.fetch_usage(&cx, &account).await
    }

    fn window(label: &str, utilization: Option<f64>, resets_at: Option<&str>) -> UsageWindow {
        UsageWindow { label: label.into(), utilization, resets_at: resets_at.map(str::to_string), value: None }
    }

    #[tokio::test]
    async fn fetch_usage_maps_windows_and_keeps_the_percent_scale() {
        let Some(pool) = test_pool().await else { return skip_without_db() };

        // REGRESSION: five_hour.utilization = 1 (meaning 1%) must stay 1 exactly.
        let usage = usage_for(&pool, json!({ "five_hour": { "utilization": 1, "resets_at": "2026-08-03T12:00:00Z" } }), json!({})).await;
        assert_eq!(usage.windows, vec![window("5h", Some(1.0), Some("2026-08-03T12:00:00Z"))]);

        // A mid-range percent passes through unchanged.
        let usage = usage_for(&pool, json!({ "five_hour": { "utilization": 73, "resets_at": null } }), json!({})).await;
        assert_eq!(usage.windows[0].utilization, Some(73.0));

        // 100 (fully used) passes through unchanged.
        let usage = usage_for(&pool, json!({ "seven_day": { "utilization": 100, "resets_at": "2026-08-10T00:00:00Z" } }), json!({})).await;
        assert_eq!(usage.windows, vec![window("Week", Some(100.0), Some("2026-08-10T00:00:00Z"))]);

        // A present window with no utilization field maps to null, not 0.
        let usage = usage_for(&pool, json!({ "five_hour": { "resets_at": "2026-08-03T12:00:00Z" } }), json!({})).await;
        assert_eq!(usage.windows, vec![window("5h", None, Some("2026-08-03T12:00:00Z"))]);

        // Both windows map to their own labeled entries.
        let usage = usage_for(
            &pool,
            json!({
                "five_hour": { "utilization": 12, "resets_at": "2026-08-03T12:00:00Z" },
                "seven_day": { "utilization": 34, "resets_at": "2026-08-10T00:00:00Z" },
            }),
            json!({}),
        )
        .await;
        assert_eq!(
            usage.windows,
            vec![window("5h", Some(12.0), Some("2026-08-03T12:00:00Z")), window("Week", Some(34.0), Some("2026-08-10T00:00:00Z"))]
        );
    }

    #[tokio::test]
    async fn fetch_usage_reads_scoped_weekly_limits_from_percent() {
        let Some(pool) = test_pool().await else { return skip_without_db() };

        // REGRESSION: a weekly_scoped limit carries its percent as `percent`, not
        // `utilization`; shape copied from a real /api/oauth/usage response.
        let usage = usage_for(
            &pool,
            json!({
                "limits": [
                    { "kind": "session", "group": "session", "percent": 22, "resets_at": "2026-08-05T00:00:00Z", "scope": null },
                    { "kind": "weekly_all", "group": "weekly", "percent": 62, "resets_at": "2026-08-05T00:00:00Z", "scope": null },
                    {
                        "kind": "weekly_scoped", "group": "weekly", "percent": 59, "severity": "normal",
                        "resets_at": "2026-08-05T00:00:00Z", "scope": { "model": { "id": null, "display_name": "Fable" } },
                        "is_active": false,
                    },
                ]
            }),
            json!({}),
        )
        .await;
        assert_eq!(usage.windows, vec![window("Fable", Some(59.0), Some("2026-08-05T00:00:00Z"))]);

        // Labeled from scope.model.display_name; non-weekly_scoped entries are ignored.
        let usage = usage_for(
            &pool,
            json!({
                "limits": [
                    { "kind": "weekly_scoped", "percent": 42, "resets_at": "2026-08-05T00:00:00Z", "scope": { "model": { "display_name": "Claude Opus 5" } } },
                    { "kind": "something_else", "percent": 99 },
                ]
            }),
            json!({}),
        )
        .await;
        assert_eq!(usage.windows, vec![window("Claude Opus 5", Some(42.0), Some("2026-08-05T00:00:00Z"))]);

        // No display name falls back to the label "scoped".
        let usage = usage_for(&pool, json!({ "limits": [{ "kind": "weekly_scoped", "percent": 5 }] }), json!({})).await;
        assert_eq!(usage.windows, vec![window("scoped", Some(5.0), None)]);

        // Neither percent nor utilization maps to null, not 0.
        let usage = usage_for(
            &pool,
            json!({ "limits": [{ "kind": "weekly_scoped", "scope": { "model": { "display_name": "Fable" } } }] }),
            json!({}),
        )
        .await;
        assert_eq!(usage.windows, vec![window("Fable", None, None)]);
    }

    #[tokio::test]
    async fn fetch_usage_account_meta_comes_from_the_profile_response() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let usage = usage_for(
            &pool,
            json!({ "five_hour": { "utilization": 10 } }),
            json!({
                "account": { "email": "user@example.com" },
                "organization": { "organization_type": "claude_max", "rate_limit_tier": "tier_4" },
            }),
        )
        .await;
        assert_eq!(
            Value::Object(usage.account),
            json!({ "email": "user@example.com", "plan_type": "claude_max", "rate_limit_tier": "tier_4" })
        );
    }

    #[tokio::test]
    async fn fetch_usage_account_email_falls_back_to_the_credential() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        stub_usage(&transport, json!({ "five_hour": { "utilization": 10 } }), json!({}), StatusCode::OK, StatusCode::OK);
        let cx = test_state(pool.clone(), transport.clone());
        let mut account = account(&pool).await;
        account.credential.email = Some("cred@example.com".into());

        let usage = ClaudeCodeAdapter.fetch_usage(&cx, &account).await;
        assert_eq!(usage.account.get("email"), Some(&json!("cred@example.com")));
    }

    #[tokio::test]
    async fn fetch_usage_non_ok_returns_stale_with_the_documented_error_string() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        stub_usage(&transport, json!({}), json!({}), StatusCode::TOO_MANY_REQUESTS, StatusCode::OK);
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;

        let usage = ClaudeCodeAdapter.fetch_usage(&cx, &account).await;
        assert!(usage.windows.is_empty());
        assert!(usage.account.is_empty());
        assert!(usage.stale);
        assert_eq!(usage.error.as_deref(), Some("usage 429"));
        assert!(!usage.edge_blocked);
    }

    // ------------------------------------------------------------------- listModels

    #[tokio::test]
    async fn list_models_maps_ids_and_reports_a_non_ok_status() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "data": [{ "id": "claude-opus-5", "display_name": "Opus 5" }, { "id": "" }, { "display_name": "no id" }] }));
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;

        let listed = ClaudeCodeAdapter.list_models(&cx, &account).await;
        assert_eq!(listed.models, vec![UpstreamModel { id: "claude-opus-5".into(), display_name: Some("Opus 5".into()) }]);
        assert_eq!(listed.error, None);
        assert_eq!(transport.requests()[0].url, "https://api.anthropic.com/v1/models?limit=100");
        assert_eq!(transport.requests()[0].header("anthropic-beta"), Some("oauth-2025-04-20"));

        let transport = MockTransport::new();
        transport.respond_json(StatusCode::UNAUTHORIZED, json!({}));
        let cx = test_state(pool.clone(), transport.clone());
        let listed = ClaudeCodeAdapter.list_models(&cx, &account).await;
        assert!(listed.models.is_empty());
        assert_eq!(listed.error.as_deref(), Some("models 401"));
    }

    // ------------------------------------------------------------- refreshIfNeeded

    #[tokio::test]
    async fn refresh_if_needed_exchanges_an_expired_credential() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "access_token": "new-token", "expires_in": 3600 }));
        let cx = test_state(pool.clone(), transport.clone());
        let user = insert_user(&pool, "cc-refresh@example.com").await;
        let credential = StoredCredential {
            access_token: "old".into(),
            refresh_token: Some("rt".into()),
            expires_at: Some("2020-01-01T00:00:00.000Z".into()),
            ..Default::default()
        };
        let row = insert_account(&pool, &user.id, "claude-code", &credential).await;

        let refreshed = ClaudeCodeAdapter.refresh_if_needed(&cx, AcquiredAccount { row, credential }).await.unwrap();
        assert_eq!(refreshed.credential.access_token, "new-token");
        assert_eq!(refreshed.credential.refresh_token.as_deref(), Some("rt"));
        let sent = transport.requests()[0].json();
        assert_eq!(transport.requests()[0].url, OAUTH_TOKEN);
        assert_eq!(sent["grant_type"], json!("refresh_token"));
        assert_eq!(sent["refresh_token"], json!("rt"));
        assert_eq!(sent["client_id"], json!(DEFAULT_CLIENT_ID));
    }

    #[tokio::test]
    async fn refresh_if_needed_leaves_a_credential_without_a_refresh_token_alone() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let cx = test_state(pool.clone(), transport.clone());
        let account = account(&pool).await;
        let out = ClaudeCodeAdapter.refresh_if_needed(&cx, account).await.unwrap();
        assert_eq!(out.credential.access_token, "tok_test");
        assert!(transport.requests().is_empty());
    }
}
