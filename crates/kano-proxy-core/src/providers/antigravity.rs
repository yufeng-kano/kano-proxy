//! Google Antigravity — Gemini models behind a Google AI Pro / Ultra subscription, reached
//! through the CloudCode internal API (`v1internal:*`). Port of
//! apps/api/src/providers/antigravity.ts; contract and caveats: docs/providers.md
//! § Antigravity.
//!
//! Wire details are derived from CLIProxyAPI (MIT), which is the source of truth for this
//! undocumented backend — each non-obvious choice cites the file it came from. Both public
//! surfaces are conversions (`proxy::gemini_openai`, `proxy::gemini_anthropic`); there is no
//! native passthrough here.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, StatusCode};
use rand::{Rng, RngCore};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::app::now_ms;
use crate::auth::provider_oauth::{antigravity_oauth_client, AntigravityOAuth};
use crate::db::accounts::parse_iso_ms;
use crate::pool::acquire::{save_credential, AcquiredAccount, StoredCredential};
use crate::proxy::gemini_anthropic::{
    anthropic_to_gemini_request, gemini_response_to_anthropic, gemini_sse_to_anthropic_stream,
};
use crate::proxy::gemini_openai::{gemini_response_to_openai, gemini_sse_to_openai_stream, openai_to_gemini_request};
use crate::upstream::transport::{TransportError, UpstreamRequest, UpstreamResponse};
use crate::AppState;

use super::antigravity_limits::{antigravity_bench_until, is_antigravity_no_capacity};
use super::refresh::refresh_oauth_credential;
use super::types::{
    AdapterError, AudioInput, CallExtras, ChatCompletionRequest, DynAdapter, FetchedUsage, ListedModels,
    ProviderAdapter, UpstreamModel, UsageWindow,
};

/// The adapter states the bench duration in this header on the failed response it hands back,
/// because Antigravity states its limit in the *body* and only the adapter can classify it;
/// `routing::feedback` prefers the hint over everything else (docs/providers.md
/// § Antigravity). Mirrors `RATELIMIT_RESET_HINT_HEADER` in apps/api/src/routing/feedback.ts.
const RATELIMIT_RESET_HINT_HEADER: &str = "x-kano-ratelimit-reset";

/// Tried in order. `daily-` is the one the desktop app prefers and is the higher-capacity of
/// the two; `cloudcode-pa` is the fallback. CLIProxyAPI `antigravityBaseURLFallbackOrder`
/// (executor_request.go).
const BASE_URLS: [&str; 2] =
    ["https://daily-cloudcode-pa.googleapis.com", "https://cloudcode-pa.googleapis.com"];

const GENERATE_PATH: &str = "/v1internal:generateContent";
const STREAM_PATH: &str = "/v1internal:streamGenerateContent?alt=sse";
const COUNT_TOKENS_PATH: &str = "/v1internal:countTokens";
const LOAD_CODE_ASSIST_PATH: &str = "/v1internal:loadCodeAssist";
const RETRIEVE_QUOTA_PATH: &str = "/v1internal:retrieveUserQuotaSummary";
const ONBOARD_USER_PATH: &str = "/v1internal:onboardUser";
const MODELS_PATH: &str = "/v1internal:fetchAvailableModels";

/// The data-plane identity is the Antigravity **CLI**, not the Hub/IDE that CLIProxyAPI's
/// `AntigravityUserAgent()` (misc/antigravity_version.go) builds. The backend gates its model
/// catalog on this header — measured 2026-08-26 through a logging relay in front of
/// `fetchAvailableModels`, same account, same token, same body, UA the only variable:
///
///   CLI form → 28 models, incl. `gemini-3.7-flash-{high,medium,low}`,
///              `defaultAgentModelId: "gemini-3.7-flash-high"`
///   hub form → 25 models, those three absent (only `-tiered` left),
///              `defaultAgentModelId: "gemini-3.6-flash-high"`
///
/// `experimentIds` was byte-identical between the two, so it is UA gating, not
/// account/experiment gating. Antigravity encodes reasoning effort in the model id, so under
/// the hub UA the effort-tiered ids are not callable at all — this divergence from
/// CLIProxyAPI is deliberate, do not "fix" it back.
///
/// Version and build are pinned (an extra network call per request to learn a version string
/// is not worth it); `ANTIGRAVITY_CLIENT_VERSION` and `ANTIGRAVITY_CLIENT_BUILD` exist for
/// when the pins go stale. docs/providers.md § Antigravity.
const FALLBACK_CLIENT_VERSION: &str = "1.1.21";
const FALLBACK_CLIENT_BUILD: &str = "970856724";
const CLIENT_OS_TYPE: &str = "darwin";
const CLIENT_ARCH: &str = "arm64";
const CLIENT_AUTH_METHOD: &str = "consumer";

/// `onboardUser` keeps the Hub identity: that control-plane call is verified working in this
/// exact shape and the catalog experiment above says nothing about it. Pinned separately via
/// `ANTIGRAVITY_HUB_VERSION`.
const FALLBACK_HUB_VERSION: &str = "2.2.1";
const CLIENT_PLATFORM: &str = "darwin/arm64";
/// The long control-plane UA `onboardUser` expects.
const NODE_API_CLIENT_UA: &str = "google-api-nodejs-client/10.3.0";
const GOOG_API_CLIENT_UA: &str = "gl-node/22.21.1";

/// Bound side fetches (refresh / models / usage) so a hung Google edge cannot stall a worker.
const SIDE_FETCH_TIMEOUT: Duration = Duration::from_millis(10_000);
/// `onboardUser` is a long-running operation and is only ever run during login.
const ONBOARD_TIMEOUT: Duration = Duration::from_millis(30_000);

fn client_version(cx: &AppState) -> String {
    cx.config().antigravity_client_version.clone().unwrap_or_else(|| FALLBACK_CLIENT_VERSION.to_string())
}

fn hub_version(cx: &AppState) -> String {
    cx.config().antigravity_hub_version.clone().unwrap_or_else(|| FALLBACK_HUB_VERSION.to_string())
}

/// CLI identity — every data-plane call. Unlocks the effort-tiered model ids.
fn cli_user_agent(cx: &AppState) -> String {
    let build = cx.config().antigravity_client_build.clone().unwrap_or_else(|| FALLBACK_CLIENT_BUILD.to_string());
    format!(
        "antigravity/cli/{} (aidev_client; os_type={CLIENT_OS_TYPE}; arch={CLIENT_ARCH}; cl={build}; auth_method={CLIENT_AUTH_METHOD})",
        client_version(cx)
    )
}

/// Hub identity — `onboardUser` only, which also appends the node client UA.
fn hub_user_agent(cx: &AppState) -> String {
    format!("antigravity/hub/{} {CLIENT_PLATFORM}", hub_version(cx))
}

fn api_request(cx: &AppState, url: String, access_token: &str, accept: &str) -> UpstreamRequest {
    UpstreamRequest::post(url)
        .header("authorization", &format!("Bearer {access_token}"))
        .header("content-type", "application/json")
        .header("accept", accept)
        .header("user-agent", &cli_user_agent(cx))
}

// ── OAuth refresh ──────────────────────────────────────────────────────────

async fn refresh_antigravity(cx: &AppState, account: AcquiredAccount) -> AcquiredAccount {
    refresh_oauth_credential(
        cx,
        account,
        |credential| {
            if credential.refresh_token.is_none() {
                return false;
            }
            let exp = credential.expires_at.as_deref().and_then(parse_iso_ms).unwrap_or(0);
            // Google access tokens live an hour; refresh with five minutes to spare.
            exp == 0 || exp - 300_000 <= now_ms()
        },
        |credential| {
            let cx = cx.clone();
            let credential = credential.clone();
            async move { refresh_antigravity_credential(&cx, credential).await }
        },
    )
    .await
}

async fn refresh_antigravity_credential(cx: &AppState, credential: StoredCredential) -> Option<StoredCredential> {
    // Confidential client: Google rejects a refresh without the secret, and this deploy has
    // none unless the operator configured the pair. Keep the stored credential rather than
    // burning the refresh token on a call that cannot succeed — the eventual 401 benches the
    // account with a real cause.
    let (configured_id, configured_secret) = antigravity_oauth_client(cx)?;
    let refresh_token = credential.refresh_token.clone()?;
    let client_id = credential.client_id.clone().filter(|v| !v.is_empty()).unwrap_or(configured_id);
    let url = credential.token_endpoint.clone().filter(|v| !v.is_empty()).unwrap_or_else(|| AntigravityOAuth::TOKEN_URL.to_string());
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", &refresh_token)
        .append_pair("client_id", &client_id)
        .append_pair("client_secret", &configured_secret)
        .finish();
    let request = UpstreamRequest::post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .timeout(SIDE_FETCH_TIMEOUT);
    let response = cx.transport().send(request).await.ok()?;
    if !response.status.is_success() {
        return None;
    }
    let json = response.json_value().await.ok()?;
    let access_token = json.get("access_token")?.as_str()?.to_string();
    let expires_in = json.get("expires_in").and_then(Value::as_i64);
    Some(StoredCredential {
        access_token,
        // Google does not rotate the refresh token on this grant, and omits it from the
        // response — keep the stored one rather than nulling it.
        refresh_token: json
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(credential.refresh_token.clone()),
        expires_at: match expires_in {
            Some(seconds) => Some(crate::db::accounts::iso_from_ms(now_ms() + seconds * 1000)),
            None => credential.expires_at.clone(),
        },
        ..credential
    })
}

// ── CloudCode project bootstrap ────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AntigravityProject {
    pub project_id: String,
    pub tier_id: Option<String>,
}

fn extract_project(payload: &Value) -> String {
    if !payload.is_object() {
        return String::new();
    }
    for key in ["cloudaicompanionProject", "projectId", "project"] {
        let Some(value) = payload.get(key) else { continue };
        if let Some(text) = value.as_str() {
            if !text.trim().is_empty() {
                return text.trim().to_string();
            }
        }
        if value.is_object() {
            if let Some(id) = value.get("id").and_then(Value::as_str) {
                if !id.trim().is_empty() {
                    return id.trim().to_string();
                }
            }
        }
    }
    String::new()
}

/// `allowedTiers[].isDefault`, else `currentTier.id`, else Google's own free tier id.
fn default_tier_id(payload: &Value) -> String {
    if !payload.is_object() {
        return "free-tier".into();
    }
    if let Some(tiers) = payload.get("allowedTiers").and_then(Value::as_array) {
        for tier in tiers {
            if !tier.is_object() {
                continue;
            }
            if tier.get("isDefault") == Some(&Value::Bool(true)) {
                if let Some(id) = tier.get("id").and_then(Value::as_str) {
                    if !id.trim().is_empty() {
                        return id.trim().to_string();
                    }
                }
            }
        }
    }
    if let Some(id) = payload.get("currentTier").and_then(|t| t.get("id")).and_then(Value::as_str) {
        if !id.trim().is_empty() {
            return id.trim().to_string();
        }
    }
    "free-tier".into()
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadCodeAssist {
    pub project_id: String,
    pub tier_id: String,
    /// `paidTier.availableCredits` Google One AI balance, when the tier reports one —
    /// measured on Google AI Pro, the matching entry carries only
    /// `minimumCreditAmountForUsage` and no `creditAmount`, so `None` is the common case, not
    /// an error. That floor is deliberately not read either: it would be a usability gate on
    /// credits whose semantics are unverified (docs/providers.md § Antigravity).
    pub credits: Option<f64>,
    /// `paidTier.name` — the plan's own name, preferred over the id for display.
    pub paid_tier_name: Option<String>,
    pub paid_tier_id: Option<String>,
}

/// A JavaScript `Number(value)` over the JSON values Google puts in `creditAmount`: the
/// balance ships as a string on the wire.
fn number_of(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|n| n.is_finite())
}

/// Reads a bounded error body for a message: trimmed, at most 200 characters, else the bare
/// status.
async fn error_detail(response: UpstreamResponse) -> String {
    let status = response.status.as_u16();
    let text = response.text().await.unwrap_or_default();
    let trimmed = text.trim();
    let detail: String = trimmed.chars().take(200).collect();
    if detail.is_empty() {
        format!("HTTP {status}")
    } else {
        detail
    }
}

/// `v1internal:loadCodeAssist` — the account's CloudCode project, tier, and (for paid tiers)
/// its Google One AI credit balance. CLIProxyAPI `internal/auth/antigravity/auth.go`
/// `FetchProjectID` for the project/tier halves, `antigravity_executor_credits.go` for the
/// credit half.
pub async fn load_code_assist(cx: &AppState, access_token: &str) -> anyhow::Result<LoadCodeAssist> {
    let request = api_request(cx, format!("{}{LOAD_CODE_ASSIST_PATH}", BASE_URLS[1]), access_token, "*/*")
        .body(Bytes::from(json!({ "metadata": { "ideType": "ANTIGRAVITY" } }).to_string()))
        .timeout(SIDE_FETCH_TIMEOUT);
    let response = cx.transport().send(request).await?;
    if !response.status.is_success() {
        let status = response.status.as_u16();
        anyhow::bail!("loadCodeAssist {status}: {}", error_detail(response).await);
    }
    let json = response.json_value().await?;
    let paid_tier = json.get("paidTier");
    let mut credits = None;
    if let Some(available) = paid_tier.and_then(|t| t.get("availableCredits")).and_then(Value::as_array) {
        for credit in available {
            if !credit.is_object() {
                continue;
            }
            let credit_type = credit.get("creditType").and_then(Value::as_str).unwrap_or("").to_ascii_uppercase();
            if credit_type != "GOOGLE_ONE_AI" {
                continue;
            }
            credits = number_of(credit.get("creditAmount"));
            break;
        }
    }
    Ok(LoadCodeAssist {
        project_id: extract_project(&json),
        tier_id: default_tier_id(&json),
        credits,
        // Google ships both halves: "g1-pro-tier" and "Google AI Pro". The name is what it
        // calls the plan, so it beats title-casing the slug ourselves.
        paid_tier_name: paid_tier
            .and_then(|t| t.get("name"))
            .and_then(Value::as_str)
            .filter(|n| !n.trim().is_empty())
            .map(str::to_string),
        paid_tier_id: paid_tier.and_then(|t| t.get("id")).and_then(Value::as_str).map(str::to_string),
    })
}

/// Emission order within a group, matching the pair claude-code pushes.
const WINDOW_ORDER: [&str; 2] = ["5h", "Week"];

/// Upstream bucket names are sentences ("Five Hour Limit Remaining"); the Providers page
/// gives a window label one short column. Compress to the strings every other adapter already
/// emits, and leave anything unrecognized alone rather than mangling it.
fn window_short_name(bucket_name: &str) -> String {
    let lower = bucket_name.to_lowercase();
    if five_hour_re().is_match(&lower) {
        return "5h".into();
    }
    if lower.contains("week") {
        return "Week".into();
    }
    bucket_name.to_string()
}

fn five_hour_re() -> &'static regex::Regex {
    static RE: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"five[\s_-]*hour|\b5\s*h\b").expect("valid regex"));
    &RE
}

/// "GEMINI MODELS" → "Gemini". Every other group bundles more than one vendor ("CLAUDE AND
/// GPT MODELS"), so naming it after either would be wrong and spelling it out does not fit
/// the column — it becomes "Other". A third Google group would land there too and need its
/// own rule (docs/providers.md).
fn group_short_name(group_name: &str) -> &'static str {
    if group_name.to_lowercase().contains("gemini") {
        "Gemini"
    } else {
        "Other"
    }
}

/// One `QuotaSummaryGroup`'s buckets → [`UsageWindow`]s, appended to `out` 5h-before-Week.
/// `group_name` is `None` for the response's ungrouped top-level `buckets`, which then carry
/// no prefix.
fn collect_quota_buckets(raw: Option<&Value>, group_name: Option<&str>, out: &mut Vec<UsageWindow>) {
    let Some(entries) = raw.and_then(Value::as_array) else { return };
    let prefix = group_name.map(group_short_name);
    let mut ranked: Vec<(usize, UsageWindow)> = Vec::new();
    for entry in entries {
        if !entry.is_object() {
            continue;
        }
        if entry.get("disabled") == Some(&Value::Bool(true)) {
            continue;
        }
        let name = entry
            .get("displayName")
            .and_then(Value::as_str)
            .filter(|n| !n.trim().is_empty())
            .or_else(|| entry.get("bucketId").and_then(Value::as_str).filter(|n| !n.trim().is_empty()));
        let Some(name) = name else { continue };
        let short = window_short_name(name);
        let rank = WINDOW_ORDER.iter().position(|w| *w == short).unwrap_or(WINDOW_ORDER.len());
        // `remaining_fraction` and `remaining_amount` are one oneof: only the fraction
        // carries a denominator, so a bucket reporting the bare amount gets a null
        // utilization rather than an invented percentage.
        let utilization = number_of(entry.get("remainingFraction")).map(|f| ((1.0 - f) * 100.0).clamp(0.0, 100.0));
        ranked.push((
            rank,
            UsageWindow {
                label: match prefix {
                    Some(prefix) => format!("{prefix} {short}"),
                    None => short,
                },
                utilization,
                resets_at: entry.get("resetTime").and_then(Value::as_str).map(str::to_string),
                value: None,
            },
        ));
    }
    ranked.sort_by_key(|(rank, _)| *rank);
    out.extend(ranked.into_iter().map(|(_, window)| window));
}

/// `v1internal:retrieveUserQuotaSummary` — the per-group quota the Antigravity CLI prints
/// under `/model`. CLIProxyAPI does not implement this endpoint; the wire shape is read from
/// the CLI's embedded protobuf descriptor (`…v1internal.QuotaSummary*`) — see
/// docs/providers.md § Antigravity.
pub async fn retrieve_user_quota_summary(
    cx: &AppState,
    access_token: &str,
    project: &str,
) -> anyhow::Result<Vec<UsageWindow>> {
    let request = api_request(cx, format!("{}{RETRIEVE_QUOTA_PATH}", BASE_URLS[1]), access_token, "*/*")
        .body(Bytes::from(json!({ "project": project }).to_string()))
        .timeout(SIDE_FETCH_TIMEOUT);
    let response = cx.transport().send(request).await?;
    if !response.status.is_success() {
        let status = response.status.as_u16();
        anyhow::bail!("retrieveUserQuotaSummary {status}: {}", error_detail(response).await);
    }
    let json = response.json_value().await?;
    let mut windows = Vec::new();
    collect_quota_buckets(json.get("buckets"), None, &mut windows);
    if let Some(groups) = json.get("groups").and_then(Value::as_array) {
        for group in groups {
            if !group.is_object() {
                continue;
            }
            let name = group.get("displayName").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty());
            collect_quota_buckets(group.get("buckets"), name, &mut windows);
        }
    }
    Ok(windows)
}

/// `v1internal:onboardUser` — creates the CloudCode project when `loadCodeAssist` returned
/// none. It is a long-running operation: poll until `done`. CLIProxyAPI polls five times at
/// 2s; a dispatch-path request cannot afford that latency, so this only ever runs during
/// login.
pub async fn onboard_user(cx: &AppState, access_token: &str, tier_id: &str) -> anyhow::Result<String> {
    let ua = format!("{} {NODE_API_CLIENT_UA}", hub_user_agent(cx));
    let body = Bytes::from(
        json!({
            "tier_id": tier_id,
            "metadata": { "ide_type": "ANTIGRAVITY", "ide_version": hub_version(cx), "ide_name": "antigravity" },
        })
        .to_string(),
    );
    for attempt in 0..5 {
        let request = api_request(cx, format!("{}{ONBOARD_USER_PATH}", BASE_URLS[0]), access_token, "*/*")
            .header("user-agent", &ua)
            .header("x-goog-api-client", GOOG_API_CLIENT_UA)
            .body(body.clone())
            .timeout(ONBOARD_TIMEOUT);
        let response = cx.transport().send(request).await?;
        if !response.status.is_success() {
            let status = response.status.as_u16();
            anyhow::bail!("onboardUser {status}: {}", error_detail(response).await);
        }
        let json = response.json_value().await?;
        if json.get("done") == Some(&Value::Bool(true)) {
            let project_id = extract_project(json.get("response").unwrap_or(&Value::Null));
            if !project_id.is_empty() {
                return Ok(project_id);
            }
            anyhow::bail!("onboardUser completed without a project id");
        }
        let _ = attempt;
        tokio::time::sleep(Duration::from_millis(2000)).await;
    }
    anyhow::bail!("onboardUser did not complete")
}

/// Project id + tier for a fresh account, run once during login.
pub async fn bootstrap_antigravity_project(cx: &AppState, access_token: &str) -> anyhow::Result<AntigravityProject> {
    let loaded = load_code_assist(cx, access_token).await?;
    if !loaded.project_id.is_empty() {
        return Ok(AntigravityProject { project_id: loaded.project_id, tier_id: Some(loaded.tier_id) });
    }
    let project_id = onboard_user(cx, access_token, &loaded.tier_id).await?;
    Ok(AntigravityProject { project_id, tier_id: Some(loaded.tier_id) })
}

fn stored_project(credential: &StoredCredential) -> String {
    credential
        .extra
        .as_ref()
        .and_then(|extra| extra.get("project_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The project id lives inside the encrypted credential payload — it is account-scoped state,
/// not a new column (docs/database.md). An account bound before the id was resolvable (a
/// manual import, or a login that raced the project's creation) fills it in here, once, and
/// persists it.
async fn ensure_project(cx: &AppState, account: AcquiredAccount) -> anyhow::Result<AcquiredAccount> {
    if !stored_project(&account.credential).is_empty() {
        return Ok(account);
    }
    let project = bootstrap_antigravity_project(cx, &account.credential.access_token).await?;
    let mut credential = account.credential.clone();
    let mut extra = credential.extra.clone().unwrap_or_default();
    extra.insert("project_id".into(), json!(project.project_id));
    extra.insert("tier_id".into(), json!(project.tier_id));
    credential.extra = Some(extra);
    save_credential(cx, &account.row.id, &credential, None).await?;
    Ok(AcquiredAccount { row: account.row, credential })
}

// ── Upstream request ───────────────────────────────────────────────────────

fn random_hex_bytes(n: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

fn random_uuid() -> String {
    let mut b = random_hex_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(&b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Keeps one conversation on one backend shard so its prompt cache hits. CLIProxyAPI
/// `generateStableSessionID` hashes the first user text part and formats it as a negative
/// int63 — the same derivation, so the same conversation gets the same id whichever proxy
/// sent it.
fn stable_session_id(request: &Map<String, Value>) -> String {
    let text = request
        .get("contents")
        .and_then(Value::as_array)
        .and_then(|contents| contents.iter().find(|c| c.get("role").and_then(Value::as_str) == Some("user")))
        .and_then(|first| first.get("parts"))
        .and_then(Value::as_array)
        .and_then(|parts| {
            parts.iter().find_map(|p| p.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()))
        });
    let Some(text) = text else {
        return format!("-{}", rand::thread_rng().gen_range(0..9_000_000_000_000_000u64));
    };
    let digest = Sha256::digest(text.as_bytes());
    // Top 8 bytes big-endian, masked to 63 bits.
    let mut top = [0u8; 8];
    top.copy_from_slice(&digest[0..8]);
    let value = u64::from_be_bytes(top) & 0x7fff_ffff_ffff_ffff;
    format!("-{value}")
}

/// Falsy in the JavaScript sense for the `sessionId` check: absent, null or empty string.
fn is_falsy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Bool(b)) => !b,
        _ => false,
    }
}

/// Wraps a Gemini request in the CloudCode envelope. CLIProxyAPI `geminiToAntigravity`
/// (executor_request.go): the model, project and `userAgent: "antigravity"` sit outside
/// `request`, and `requestType` is `agent` for everything but the image models.
pub fn build_antigravity_envelope(
    model: &str,
    project_id: &str,
    request: Value,
    session_id: Option<&str>,
) -> Value {
    let mut request = match request {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let is_image_model = model.contains("image");
    if is_falsy(request.get("sessionId")) {
        let derived = match session_id.filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => stable_session_id(&request),
        };
        request.insert("sessionId".into(), Value::String(derived));
    }

    // Claude models served through Antigravity need VALIDATED function calling when tools are
    // present, and Gemini models reject `maxOutputTokens` alongside tools or a response
    // schema. Both rules are CLIProxyAPI `buildRequest`'s, kept because the backend 400s
    // without them and neither can be probed for free. VALIDATED must not be attached to a
    // tool-less request (e.g. structured output only) — an unattached function-calling config
    // is itself rejected.
    let is_claude = model.to_lowercase().contains("claude");
    let has_tools = request.get("tools").and_then(Value::as_array).is_some_and(|t| !t.is_empty());
    let has_schema = has_tools
        || request.get("generationConfig").and_then(|g| g.get("responseSchema")).is_some_and(|s| !s.is_null());
    if is_claude {
        if has_tools {
            let existing = request
                .get("toolConfig")
                .and_then(|c| c.get("functionCallingConfig"))
                .and_then(Value::as_object)
                .cloned();
            let mode = existing
                .as_ref()
                .and_then(|c| c.get("mode"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // VALIDATED replaces only the default AUTO. An explicit client choice — NONE
            // (never call) or ANY (forced call, possibly with allowedFunctionNames) — is a
            // semantic the proxy must not overwrite: doing so could produce a tool call the
            // client prohibited, or text where a call was required.
            if mode.is_empty() || mode == "AUTO" {
                let mut config = existing.unwrap_or_default();
                config.insert("mode".into(), json!("VALIDATED"));
                let mut tool_config = request
                    .get("toolConfig")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                tool_config.insert("functionCallingConfig".into(), Value::Object(config));
                request.insert("toolConfig".into(), Value::Object(tool_config));
            }
        }
    } else if has_schema {
        if let Some(Value::Object(config)) = request.get_mut("generationConfig") {
            config.remove("maxOutputTokens");
        }
    }

    let mut envelope = Map::new();
    envelope.insert("model".into(), json!(model));
    if !project_id.is_empty() {
        envelope.insert("project".into(), json!(project_id));
    }
    envelope.insert("userAgent".into(), json!("antigravity"));
    envelope.insert("requestType".into(), json!(if is_image_model { "image_gen" } else { "agent" }));
    envelope.insert("requestId".into(), json!(format!("agent-{}", random_uuid())));
    envelope.insert("request".into(), Value::Object(request));
    Value::Object(envelope)
}

struct FailedUpstream {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

enum UpstreamResult {
    Ok(UpstreamResponse),
    Failed(FailedUpstream),
}

/// POST to the first base URL that answers, falling back on a transport error, a 429, or an
/// explicit "no capacity". The fallback list is the only retry: a 429 that survives both base
/// URLs is the routing module's problem, not a loop this adapter runs (docs/providers.md
/// § Antigravity).
async fn post_with_fallback(
    cx: &AppState,
    access_token: &str,
    path: &str,
    body: &Value,
    stream: bool,
    extras: &CallExtras,
) -> Result<UpstreamResult, AdapterError> {
    let accept = if stream { "text/event-stream" } else { "*/*" };
    let payload = Bytes::from(serde_json::to_vec(body).map_err(|e| AdapterError::Other(e.into()))?);
    let mut last: Option<FailedUpstream> = None;
    let mut last_error: Option<TransportError> = None;

    for (index, base) in BASE_URLS.iter().enumerate() {
        let mut request =
            api_request(cx, format!("{base}{path}"), access_token, accept).body(payload.clone());
        if let Some(timeout) = extras.first_byte_timeout {
            request = request.timeout(timeout);
        }
        let response = match cx.transport().send(request).await {
            Ok(response) => response,
            Err(e) => {
                // A transport failure never wins over an earlier real HTTP answer: the saved
                // response still carries the body dispatch needs for feedback (bench +
                // candidate failover), which a thrown error would erase.
                last_error = Some(e);
                continue;
            }
        };

        if response.status.is_success() {
            return Ok(UpstreamResult::Ok(response));
        }

        // Read the error body once: it decides both the fallback and, for a 429, how long the
        // account is benched.
        let status = response.status;
        let headers = response.headers.clone();
        let text = response.bytes().await.unwrap_or_default();
        last = Some(FailedUpstream { status, headers, body: text.clone() });
        if index + 1 >= BASE_URLS.len() {
            break;
        }
        // A non-JSON error body cannot say "no capacity".
        let parsed: Value = serde_json::from_slice(&text).unwrap_or(Value::Null);
        if status.as_u16() == 429 || is_antigravity_no_capacity(status.as_u16(), &parsed) {
            continue;
        }
        break;
    }

    if let Some(last) = last {
        return Ok(UpstreamResult::Failed(last));
    }
    Err(match last_error {
        Some(e) => AdapterError::Transport(e),
        None => AdapterError::Other(anyhow::anyhow!("antigravity: no base url available")),
    })
}

/// Rebuilds a failed upstream response for dispatch, attaching the reset hint when the 429
/// body says how long to wait. The body is passed through verbatim so the client sees
/// Google's own error.
fn error_response(result: FailedUpstream) -> Response {
    let content_type = result
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .unwrap_or("application/json")
        .to_string();
    let mut builder = Response::builder().status(result.status).header(CONTENT_TYPE, content_type);
    if result.status.as_u16() == 429 && !result.body.is_empty() {
        // A body that does not parse keeps the default bench.
        let parsed: Value = serde_json::from_slice(&result.body).unwrap_or(Value::Null);
        if is_antigravity_no_capacity(result.status.as_u16(), &parsed) {
            // "No capacity" is a fleet-side condition, not this credential's fault
            // (docs/providers.md § Antigravity): a 429 that survived both base URLs must not
            // bench the account for the routing module's 300s default. An already-expired
            // reset hint keeps the ordinary failover walk (try the next candidate now) while
            // making the recorded bench a no-op.
            builder = builder.header(RATELIMIT_RESET_HINT_HEADER, now_ms().to_string());
        } else if let Some(until) = antigravity_bench_until(&parsed, now_ms()) {
            builder = builder.header(RATELIMIT_RESET_HINT_HEADER, until.to_string());
        }
    }
    builder.body(Body::from(result.body)).expect("response builds")
}

fn json_response(status: StatusCode, value: &Value) -> Response {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(value).unwrap_or_default()))
        .expect("response builds")
}

fn sse_response(stream: crate::upstream::transport::ByteStream) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header("cache-control", "no-cache")
        .body(Body::from_stream(stream))
        .expect("response builds")
}

fn invalid_effort_response() -> Response {
    json_response(
        StatusCode::BAD_REQUEST,
        &json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "invalid reasoning_effort" } }),
    )
}

// ── Adapter ────────────────────────────────────────────────────────────────

pub struct AntigravityAdapter;

pub fn adapter() -> DynAdapter {
    Arc::new(AntigravityAdapter)
}

#[async_trait]
impl ProviderAdapter for AntigravityAdapter {
    fn id(&self) -> &str {
        "antigravity"
    }

    /// Gemini reads audio as an ordinary `inlineData` part (docs/api.md § Audio input).
    fn audio_input(&self) -> Option<AudioInput> {
        Some(AudioInput::Convert)
    }

    async fn refresh_if_needed(&self, cx: &AppState, account: AcquiredAccount) -> Result<AcquiredAccount, AdapterError> {
        // Project bootstrap is part of making this credential usable, so it runs here: a
        // bootstrap failure (loadCodeAssist / onboardUser rejecting) stays account-scoped —
        // dispatch skips the candidate and continues the walk, same as an unreadable
        // credential — instead of failing inside the request method and turning into a
        // request-terminal 502 that blocks every account after this one in the pool.
        let refreshed = refresh_antigravity(cx, account).await;
        ensure_project(cx, refreshed).await.map_err(AdapterError::Other)
    }

    /// `/openai/v1`: Chat Completions ↔ Gemini `GenerateContent`.
    async fn chat_completions(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        req: &ChatCompletionRequest,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let acc = refresh_antigravity(cx, account.clone()).await;
        let acc = ensure_project(cx, acc).await.map_err(AdapterError::Other)?;

        let session_id = req
            .prompt_cache_key
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| req.affinity.as_ref().and_then(|a| a.conv_id.clone()).filter(|s| !s.is_empty()));
        let envelope = build_antigravity_envelope(
            &req.upstream_model,
            &stored_project(&acc.credential),
            openai_to_gemini_request(req),
            session_id.as_deref(),
        );

        let stream = req.stream.unwrap_or(false);
        let path = if stream { STREAM_PATH } else { GENERATE_PATH };
        let result = post_with_fallback(cx, &acc.credential.access_token, path, &envelope, stream, extras).await?;
        let response = match result {
            UpstreamResult::Failed(failed) => return Ok(error_response(failed)),
            UpstreamResult::Ok(response) => response,
        };

        if stream {
            return Ok(sse_response(gemini_sse_to_openai_stream(response.body, &req.raw_model)));
        }
        let json = response.json_value().await.map_err(|e| AdapterError::Other(e.into()))?;
        Ok(json_response(StatusCode::OK, &gemini_response_to_openai(&json, &req.raw_model)))
    }

    fn has_messages(&self) -> bool {
        true
    }

    /// `/anthropic`: Messages ↔ Gemini `GenerateContent`. A conversion, not a passthrough.
    async fn messages(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        body: &Value,
        headers: &HeaderMap,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let acc = refresh_antigravity(cx, account.clone()).await;
        let acc = ensure_project(cx, acc).await.map_err(AdapterError::Other)?;

        // The route already rewrote `model` to the bare upstream id — used as-is, never
        // re-split: a namespaced upstream id ("org/model") would lose its first segment. The
        // client-visible id it sent rides along in a header so responses echo it back.
        let upstream_model = body.get("model").and_then(Value::as_str).unwrap_or("").to_string();
        let display_model = headers
            .get("x-kano-raw-model")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("antigravity/{upstream_model}"));

        let Ok(converted) = anthropic_to_gemini_request(body) else {
            return Ok(invalid_effort_response());
        };

        let session_id = body
            .get("metadata")
            .and_then(|m| m.get("user_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let envelope = build_antigravity_envelope(
            &upstream_model,
            &stored_project(&acc.credential),
            converted.request,
            session_id.as_deref(),
        );

        let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
        let path = if stream { STREAM_PATH } else { GENERATE_PATH };
        let result = post_with_fallback(cx, &acc.credential.access_token, path, &envelope, stream, extras).await?;
        let response = match result {
            UpstreamResult::Failed(failed) => return Ok(error_response(failed)),
            UpstreamResult::Ok(response) => response,
        };

        if stream {
            return Ok(sse_response(gemini_sse_to_anthropic_stream(response.body, &display_model, converted.thinking_mode)));
        }
        let json = response.json_value().await.map_err(|e| AdapterError::Other(e.into()))?;
        Ok(json_response(
            StatusCode::OK,
            &gemini_response_to_anthropic(&json, &display_model, converted.thinking_mode),
        ))
    }

    fn has_count_tokens(&self) -> bool {
        true
    }

    /// `POST /anthropic/v1/messages/count_tokens` — a real upstream count, not an estimate.
    /// `v1internal:countTokens` takes a **bare** `{request}` body, not the CloudCode generate
    /// envelope: the envelope-only fields (`userAgent`, `requestType`, `requestId`,
    /// `request.sessionId`) and the Claude-only `VALIDATED` toolConfig injection all 400 the
    /// whole call — which is what made every Claude Code `/context` probe fail and
    /// retry-storm the account (docs/providers.md § Antigravity). CLIProxyAPI
    /// `antigravity_executor_tokens.go` is the wire reference: it never runs its
    /// generate-path `buildRequest`, and strips `model`/`project`/`safetySettings` (this
    /// converter never emits safetySettings). Answers `{totalTokens}`; `tools` are accepted
    /// but ignored by the count (CLIProxyAPI issue #840), so this can undercount tool
    /// schemas.
    async fn count_tokens(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        body: &Value,
        _headers: &HeaderMap,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        // No ensure_project: the bare countTokens body carries no project id, and
        // bootstrapping one here would add loadCodeAssist/onboardUser calls to an endpoint
        // clients fire in parallel bursts (Claude Code `/context`).
        let acc = refresh_antigravity(cx, account.clone()).await;
        // Same 400 as `messages`: a malformed client field is the client's error, never a 502
        // "upstream error" for a call that was never made.
        let Ok(converted) = anthropic_to_gemini_request(body) else {
            return Ok(invalid_effort_response());
        };
        let payload = json!({ "request": converted.request });
        let result =
            post_with_fallback(cx, &acc.credential.access_token, COUNT_TOKENS_PATH, &payload, false, extras).await?;
        let response = match result {
            UpstreamResult::Failed(failed) => return Ok(error_response(failed)),
            UpstreamResult::Ok(response) => response,
        };
        let json = response.json_value().await.map_err(|e| AdapterError::Other(e.into()))?;
        // Clients budget context on this number. A 200 whose `totalTokens` is missing or
        // malformed must surface as an upstream error, never as a plausible-but-fabricated
        // `input_tokens: 0`.
        let total = json.get("totalTokens").and_then(Value::as_f64).filter(|t| t.is_finite() && *t >= 0.0);
        let Some(total) = total else {
            return Ok(json_response(
                StatusCode::BAD_GATEWAY,
                &json!({
                    "type": "error",
                    "error": { "type": "api_error", "message": "antigravity countTokens returned no totalTokens" },
                }),
            ));
        };
        // JS numbers serialize integral values without a fraction; keep that wire shape.
        let total = if total.fract() == 0.0 && total <= i64::MAX as f64 { json!(total as i64) } else { json!(total) };
        Ok(json_response(StatusCode::OK, &json!({ "input_tokens": total })))
    }

    fn has_list_models(&self) -> bool {
        true
    }

    /// `v1internal:fetchAvailableModels` — a live map of `{id: {displayName, …}}`. There is
    /// no offline mirror for this list and no hard-coded fallback: a failure returns empty
    /// plus the error, never an invented catalog.
    async fn list_models(&self, cx: &AppState, account: &AcquiredAccount) -> ListedModels {
        let acc = refresh_antigravity(cx, account.clone()).await;
        // The catalog is normally how a user discovers a callable model, so an account whose
        // login-time project bootstrap failed retries it here — otherwise it would stay stuck
        // on "models fetch failed" until some known-id chat request happened to run the
        // bootstrap first. A bootstrap failure is still tolerated: the fetch below may work
        // without a project.
        let acc = match ensure_project(cx, acc.clone()).await {
            Ok(acc) => acc,
            Err(_) => acc,
        };
        let project_id = stored_project(&acc.credential);
        let body = if project_id.is_empty() { json!({}) } else { json!({ "project": project_id }) };
        for base in BASE_URLS {
            let request = api_request(cx, format!("{base}{MODELS_PATH}"), &acc.credential.access_token, "*/*")
                .body(Bytes::from(body.to_string()))
                .timeout(SIDE_FETCH_TIMEOUT);
            let Ok(response) = cx.transport().send(request).await else { continue };
            if !response.status.is_success() {
                continue;
            }
            let Ok(json) = response.json_value().await else { continue };
            // An array here means the undocumented endpoint changed shape — treating it as a
            // map would publish the array indexes ("0", "1", …) as callable model ids and
            // cache that invented catalog for an hour.
            let Some(models) = json.get("models").and_then(Value::as_object) else { continue };
            let models = models
                .iter()
                .filter(|(id, _)| !id.trim().is_empty())
                .map(|(id, info)| UpstreamModel {
                    id: id.clone(),
                    display_name: info
                        .get("displayName")
                        .and_then(Value::as_str)
                        .filter(|n| !n.is_empty())
                        .map(str::to_string),
                })
                .collect();
            // A well-formed `{models: {}}` is a real answer — an account with no currently
            // available models — not an upstream failure to retry or cache as an error for an
            // hour.
            return ListedModels { models, error: None };
        }
        ListedModels { models: Vec::new(), error: Some("models fetch failed".into()) }
    }

    fn has_fetch_usage(&self) -> bool {
        true
    }

    /// Two calls: `loadCodeAssist` for the tier, project and credit balance, then
    /// `retrieveUserQuotaSummary` for the real quota windows (docs/providers.md
    /// § Antigravity). The credit balance never becomes a window — it has no total and no
    /// reset — so it stays in the account metadata for the UI to print verbatim. A quota
    /// failure alone keeps the metadata and reports `stale`, which the usage cache merges
    /// over the last good windows.
    async fn fetch_usage(&self, cx: &AppState, account: &AcquiredAccount) -> FetchedUsage {
        let acc = refresh_antigravity(cx, account.clone()).await;
        let email = acc.credential.email.clone();
        let loaded = match load_code_assist(cx, &acc.credential.access_token).await {
            Ok(loaded) => loaded,
            Err(e) => {
                let mut meta = Map::new();
                meta.insert("email".into(), json!(email));
                return FetchedUsage {
                    windows: Vec::new(),
                    account: meta,
                    stale: true,
                    error: Some(e.to_string()),
                    edge_blocked: false,
                };
            }
        };
        let project = if !loaded.project_id.is_empty() {
            Some(loaded.project_id.clone())
        } else {
            Some(stored_project(&acc.credential)).filter(|p| !p.is_empty())
        };
        let mut meta = Map::new();
        meta.insert("email".into(), json!(email));
        meta.insert(
            "plan_type".into(),
            json!(loaded.paid_tier_name.clone().or_else(|| loaded.paid_tier_id.clone()).unwrap_or(loaded.tier_id.clone())),
        );
        meta.insert("project_id".into(), json!(project));
        // `is_some` and not a truthiness check: a balance of 0 is a fact worth printing, not
        // a missing one.
        if let Some(credits) = loaded.credits {
            let credits = if credits.fract() == 0.0 && credits.abs() < 9.0e15 { json!(credits as i64) } else { json!(credits) };
            meta.insert("credits_remaining".into(), json!(credits));
        }
        let Some(project) = project else {
            return FetchedUsage { windows: Vec::new(), account: meta, ..Default::default() };
        };
        match retrieve_user_quota_summary(cx, &acc.credential.access_token, &project).await {
            Ok(windows) => FetchedUsage { windows, account: meta, ..Default::default() },
            Err(e) => FetchedUsage {
                windows: Vec::new(),
                account: meta,
                stale: true,
                error: Some(e.to_string()),
                edge_blocked: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::accounts::iso_from_ms;
    use crate::db::test_support::{insert_account, insert_user, skip_without_db, test_pool, test_state};
    use crate::upstream::transport::MockTransport;
    use http::HeaderValue;
    use http_body_util::BodyExt;

    const DAILY: &str = "https://daily-cloudcode-pa.googleapis.com";
    const PROD: &str = "https://cloudcode-pa.googleapis.com";
    /// The pinned CLI identity every data-plane call must send.
    const CLI_UA: &str =
        "antigravity/cli/1.1.21 (aidev_client; os_type=darwin; arch=arm64; cl=970856724; auth_method=consumer)";

    /// A credential whose token is fresh and whose project is already resolved, so neither the
    /// refresh nor the bootstrap path fires during the test.
    fn credential() -> StoredCredential {
        let mut extra = Map::new();
        extra.insert("project_id".into(), json!("proj-42"));
        StoredCredential {
            access_token: "at-1".into(),
            refresh_token: Some("rt-1".into()),
            expires_at: Some(iso_from_ms(now_ms() + 3_600_000)),
            extra: Some(extra),
            ..Default::default()
        }
    }

    fn credential_without_project() -> StoredCredential {
        StoredCredential { extra: None, ..credential() }
    }

    /// `(state, account)` on a fresh test database; `None` when no test database answers.
    async fn setup(transport: Arc<MockTransport>, credential: StoredCredential) -> Option<(AppState, AcquiredAccount)> {
        let pool = test_pool().await?;
        let state = test_state(pool.clone(), transport);
        let user = insert_user(&pool, &format!("ag-{}@example.com", hex::encode(random_hex_bytes(4)))).await;
        let row = insert_account(&pool, &user.id, "antigravity", &credential).await;
        Some((state, AcquiredAccount { row, credential }))
    }

    fn chat_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "antigravity/gemini-3-flash".into(),
            raw_model: "antigravity/gemini-3-flash".into(),
            upstream_model: "gemini-3-flash".into(),
            messages: vec![json!({ "role": "user", "content": "hi" })],
            ..Default::default()
        }
    }

    fn ok_generate(text: &str) -> UpstreamResponse {
        UpstreamResponse::json(
            StatusCode::OK,
            &json!({
                "response": {
                    "candidates": [{ "content": { "role": "model", "parts": [{ "text": text }] }, "finishReason": "STOP" }],
                    "usageMetadata": { "promptTokenCount": 3, "candidatesTokenCount": 1, "totalTokenCount": 4 },
                }
            }),
        )
    }

    fn quota_429(details: Value) -> UpstreamResponse {
        UpstreamResponse::json(
            StatusCode::TOO_MANY_REQUESTS,
            &json!({ "error": { "status": "RESOURCE_EXHAUSTED", "details": details } }),
        )
    }

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.expect("body collects").to_bytes();
        serde_json::from_slice(&bytes).expect("json body")
    }

    fn headers_with_raw_model(raw: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-kano-raw-model", HeaderValue::from_str(raw).unwrap());
        headers
    }

    // ── buildAntigravityEnvelope ────────────────────────────────────────────

    #[test]
    fn puts_model_project_and_the_fixed_user_agent_outside_the_gemini_request() {
        let envelope = build_antigravity_envelope(
            "gemini-3-flash",
            "proj-42",
            json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }] }),
            None,
        );
        assert_eq!(envelope["model"], json!("gemini-3-flash"));
        assert_eq!(envelope["project"], json!("proj-42"));
        assert_eq!(envelope["userAgent"], json!("antigravity"));
        assert_eq!(envelope["requestType"], json!("agent"));
        assert!(envelope["requestId"].as_str().unwrap().starts_with("agent-"));
        assert_eq!(envelope["request"]["contents"], json!([{ "role": "user", "parts": [{ "text": "hi" }] }]));
    }

    #[test]
    fn derives_a_session_id_stable_for_the_same_opening_user_text() {
        let build = |text: &str| {
            build_antigravity_envelope(
                "gemini-3-flash",
                "p",
                json!({ "contents": [{ "role": "user", "parts": [{ "text": text }] }] }),
                None,
            )["request"]["sessionId"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let a = build("same opener");
        let b = build("same opener");
        assert_eq!(a, b);
        assert!(a.starts_with('-') && a[1..].chars().all(|c| c.is_ascii_digit()));
        assert_ne!(build("different opener"), a);
    }

    #[test]
    fn prefers_an_explicit_session_id_over_the_derived_one() {
        let envelope = build_antigravity_envelope("gemini-3-flash", "p", json!({ "contents": [] }), Some("conv-7"));
        assert_eq!(envelope["request"]["sessionId"], json!("conv-7"));
    }

    #[test]
    fn omits_project_entirely_when_none_is_known() {
        let envelope = build_antigravity_envelope("gemini-3-flash", "", json!({ "contents": [] }), None);
        assert!(envelope.get("project").is_none());
    }

    #[test]
    fn drops_max_output_tokens_for_a_gemini_model_that_also_sends_tools() {
        let envelope = build_antigravity_envelope(
            "gemini-3-flash",
            "p",
            json!({
                "contents": [],
                "tools": [{ "functionDeclarations": [] }],
                "generationConfig": { "maxOutputTokens": 128, "temperature": 0.5 },
            }),
            None,
        );
        assert_eq!(envelope["request"]["generationConfig"], json!({ "temperature": 0.5 }));
    }

    #[test]
    fn keeps_max_output_tokens_when_there_is_no_schema_to_conflict_with() {
        let envelope = build_antigravity_envelope(
            "gemini-3-flash",
            "p",
            json!({ "contents": [], "generationConfig": { "maxOutputTokens": 128 } }),
            None,
        );
        assert_eq!(envelope["request"]["generationConfig"], json!({ "maxOutputTokens": 128 }));
    }

    #[test]
    fn forces_validated_function_calling_for_a_claude_model_behind_antigravity() {
        let envelope = build_antigravity_envelope(
            "claude-sonnet-4-6",
            "p",
            json!({ "contents": [], "tools": [{ "functionDeclarations": [] }] }),
            None,
        );
        assert_eq!(envelope["request"]["toolConfig"], json!({ "functionCallingConfig": { "mode": "VALIDATED" } }));
    }

    #[test]
    fn preserves_an_explicit_none_or_any_tool_choice_for_a_claude_model() {
        // Overwriting an explicit mode could produce a tool call the client prohibited (NONE)
        // or text where a call was required (ANY).
        let none = build_antigravity_envelope(
            "claude-sonnet-4-6",
            "p",
            json!({
                "contents": [],
                "tools": [{ "functionDeclarations": [] }],
                "toolConfig": { "functionCallingConfig": { "mode": "NONE" } },
            }),
            None,
        );
        assert_eq!(none["request"]["toolConfig"], json!({ "functionCallingConfig": { "mode": "NONE" } }));

        let forced = build_antigravity_envelope(
            "claude-sonnet-4-6",
            "p",
            json!({
                "contents": [],
                "tools": [{ "functionDeclarations": [] }],
                "toolConfig": { "functionCallingConfig": { "mode": "ANY", "allowedFunctionNames": ["search"] } },
            }),
            None,
        );
        assert_eq!(
            forced["request"]["toolConfig"],
            json!({ "functionCallingConfig": { "mode": "ANY", "allowedFunctionNames": ["search"] } })
        );
    }

    #[test]
    fn upgrades_the_default_auto_tool_choice_to_validated_for_a_claude_model() {
        let envelope = build_antigravity_envelope(
            "claude-sonnet-4-6",
            "p",
            json!({
                "contents": [],
                "tools": [{ "functionDeclarations": [] }],
                "toolConfig": { "functionCallingConfig": { "mode": "AUTO" } },
            }),
            None,
        );
        assert_eq!(envelope["request"]["toolConfig"], json!({ "functionCallingConfig": { "mode": "VALIDATED" } }));
    }

    #[test]
    fn does_not_force_validated_for_a_tool_less_claude_request_with_only_a_response_schema() {
        // Structured output without tools must not carry an unattached function-calling
        // config — the backend rejects that shape.
        let envelope = build_antigravity_envelope(
            "claude-sonnet-4-6",
            "p",
            json!({ "contents": [], "generationConfig": { "responseSchema": { "type": "object" } } }),
            None,
        );
        assert!(envelope["request"].get("toolConfig").is_none());
    }

    #[test]
    fn uses_the_image_request_type_for_an_image_model() {
        let envelope = build_antigravity_envelope("gemini-3.1-flash-image", "p", json!({ "contents": [] }), None);
        assert_eq!(envelope["requestType"], json!("image_gen"));
    }

    // ── refreshIfNeeded ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn rejects_on_a_failed_project_bootstrap_so_dispatch_skips_the_candidate() {
        let transport = MockTransport::new();
        transport.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new(), "boom")));
        let Some((cx, account)) = setup(transport, credential_without_project()).await else {
            return skip_without_db();
        };
        // A rejection here makes the candidate walk skip this account and keep going —
        // instead of the bootstrap failing mid-request and turning into a request-terminal
        // 502 that blocks the rest of the pool.
        let err = AntigravityAdapter.refresh_if_needed(&cx, account).await.expect_err("bootstrap fails");
        assert!(err.to_string().contains("loadCodeAssist 500"), "{err}");
    }

    #[tokio::test]
    async fn is_a_no_op_beyond_refresh_when_the_project_is_already_stored() {
        let transport = MockTransport::new();
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let out = AntigravityAdapter.refresh_if_needed(&cx, account).await.unwrap();
        assert_eq!(stored_project(&out.credential), "proj-42");
        assert!(transport.requests().is_empty());
    }

    // ── chatCompletions ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn posts_the_envelope_to_the_daily_base_url_with_the_antigravity_identity() {
        let transport = MockTransport::new();
        transport.expect(|_| Ok(ok_generate("hello")));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let res = AntigravityAdapter
            .chat_completions(&cx, &account, &chat_request(), &CallExtras::default())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let calls = transport.requests();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].url, format!("{DAILY}/v1internal:generateContent"));
        assert_eq!(calls[0].header("authorization"), Some("Bearer at-1"));
        // The CLI identity, not the Hub one: the backend gates the effort-tiered model ids on
        // it (docs/providers.md § Antigravity).
        assert_eq!(calls[0].header("user-agent"), Some(CLI_UA));
        let body = calls[0].json();
        assert_eq!(body["model"], json!("gemini-3-flash"));
        assert_eq!(body["project"], json!("proj-42"));

        // The client-visible model is echoed, not the bare upstream id.
        assert_eq!(body_json(res).await["model"], json!("antigravity/gemini-3-flash"));
    }

    #[tokio::test]
    async fn honours_the_client_version_and_build_overrides_in_the_user_agent() {
        let transport = MockTransport::new();
        transport.expect(|_| Ok(ok_generate("hello")));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let mut config = cx.config().clone();
        config.antigravity_client_version = Some("9.9.9".into());
        config.antigravity_client_build = Some("123456789".into());
        let cx = AppState::builder(config, cx.pool().clone()).transport(transport.clone()).build();

        AntigravityAdapter.chat_completions(&cx, &account, &chat_request(), &CallExtras::default()).await.unwrap();
        assert_eq!(
            transport.requests()[0].header("user-agent"),
            Some("antigravity/cli/9.9.9 (aidev_client; os_type=darwin; arch=arm64; cl=123456789; auth_method=consumer)")
        );
    }

    #[tokio::test]
    async fn uses_the_streaming_path_with_alt_sse_when_the_client_asked_to_stream() {
        let transport = MockTransport::new();
        transport.respond_sse(StatusCode::OK, "data: {\"response\":{\"candidates\":[{\"finishReason\":\"STOP\"}]}}\n\n");
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let req = ChatCompletionRequest { stream: Some(true), ..chat_request() };
        let res = AntigravityAdapter.chat_completions(&cx, &account, &req, &CallExtras::default()).await.unwrap();
        assert_eq!(transport.requests()[0].url, format!("{DAILY}/v1internal:streamGenerateContent?alt=sse"));
        assert!(res.headers()[CONTENT_TYPE].to_str().unwrap().contains("text/event-stream"));
    }

    #[tokio::test]
    async fn falls_back_to_the_second_base_url_on_a_transport_error() {
        let transport = MockTransport::new();
        transport.expect(|_| Err(TransportError::Connect("network down".into())));
        transport.expect(|_| Ok(ok_generate("hello")));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let res = AntigravityAdapter
            .chat_completions(&cx, &account, &chat_request(), &CallExtras::default())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let origins: Vec<String> =
            transport.requests().iter().map(|r| r.url.split("/v1internal").next().unwrap().to_string()).collect();
        assert_eq!(origins, vec![DAILY.to_string(), PROD.to_string()]);
    }

    #[tokio::test]
    async fn falls_back_on_a_429_then_returns_the_last_failure() {
        let transport = MockTransport::new();
        let quota = || {
            quota_429(json!([{ "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "QUOTA_EXHAUSTED" }]))
        };
        transport.expect(move |_| Ok(quota()));
        transport.expect(move |_| {
            Ok(quota_429(json!([{ "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "QUOTA_EXHAUSTED" }])))
        });
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let before = now_ms();
        let res = AntigravityAdapter
            .chat_completions(&cx, &account, &chat_request(), &CallExtras::default())
            .await
            .unwrap();
        assert_eq!(transport.requests().len(), 2);
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        // Quota exhaustion with no upstream reset benches for the documented hour.
        let hint: i64 = res.headers()[RATELIMIT_RESET_HINT_HEADER].to_str().unwrap().parse().unwrap();
        assert!(hint >= before + super::super::antigravity_limits::ANTIGRAVITY_QUOTA_BENCH_MS);
    }

    #[tokio::test]
    async fn attaches_the_upstream_retry_delay_as_the_reset_hint_on_a_transient_throttle() {
        let transport = MockTransport::new();
        let details = json!([
            { "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "RATE_LIMIT_EXCEEDED" },
            { "@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "30s" },
        ]);
        for _ in 0..2 {
            let d = details.clone();
            transport.expect(move |_| Ok(quota_429(d.clone())));
        }
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let before = now_ms();
        let res = AntigravityAdapter
            .chat_completions(&cx, &account, &chat_request(), &CallExtras::default())
            .await
            .unwrap();
        let hint: i64 = res.headers()[RATELIMIT_RESET_HINT_HEADER].to_str().unwrap().parse().unwrap();
        assert!(hint >= before + 30_000);
        assert!(hint < before + 60_000);
    }

    #[tokio::test]
    async fn returns_the_earlier_429_when_the_fallback_then_fails_at_the_transport_layer() {
        let transport = MockTransport::new();
        transport.expect(|_| {
            Ok(quota_429(json!([{ "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "QUOTA_EXHAUSTED" }])))
        });
        transport.expect(|_| Err(TransportError::Connect("network down".into())));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let before = now_ms();
        // The saved HTTP answer must survive the later transport failure — an error would
        // collapse into a generic 502 and skip the bench classification plus candidate
        // failover entirely.
        let res = AntigravityAdapter
            .chat_completions(&cx, &account, &chat_request(), &CallExtras::default())
            .await
            .unwrap();
        assert_eq!(transport.requests().len(), 2);
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        let hint: i64 = res.headers()[RATELIMIT_RESET_HINT_HEADER].to_str().unwrap().parse().unwrap();
        assert!(hint >= before + super::super::antigravity_limits::ANTIGRAVITY_QUOTA_BENCH_MS);
    }

    #[tokio::test]
    async fn does_not_bench_the_account_for_a_terminal_no_capacity_429() {
        let transport = MockTransport::new();
        let no_capacity = || {
            UpstreamResponse::json(
                StatusCode::TOO_MANY_REQUESTS,
                &json!({ "error": { "status": "RESOURCE_EXHAUSTED", "message": "no capacity" } }),
            )
        };
        transport.expect(move |_| Ok(no_capacity()));
        transport.expect(move |_| {
            Ok(UpstreamResponse::json(
                StatusCode::TOO_MANY_REQUESTS,
                &json!({ "error": { "status": "RESOURCE_EXHAUSTED", "message": "no capacity" } }),
            ))
        });
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let res = AntigravityAdapter
            .chat_completions(&cx, &account, &chat_request(), &CallExtras::default())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        // Fleet-side condition, not this credential's: the hint must not put the account on
        // the 300s default bench, only let the failover walk continue.
        let hint: i64 = res.headers()[RATELIMIT_RESET_HINT_HEADER].to_str().unwrap().parse().unwrap();
        assert!(hint <= now_ms());
    }

    #[tokio::test]
    async fn does_not_fall_back_or_hint_on_a_non_429_failure() {
        let transport = MockTransport::new();
        transport.expect(|_| {
            Ok(UpstreamResponse::json(StatusCode::NOT_FOUND, &json!({ "error": { "message": "bad model" } })))
        });
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let res = AntigravityAdapter
            .chat_completions(&cx, &account, &chat_request(), &CallExtras::default())
            .await
            .unwrap();
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert!(res.headers().get(RATELIMIT_RESET_HINT_HEADER).is_none());
        assert_eq!(body_json(res).await, json!({ "error": { "message": "bad model" } }));
    }

    // ── messages ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn converts_an_anthropic_body_and_echoes_the_client_visible_model_back() {
        let transport = MockTransport::new();
        transport.expect(|_| Ok(ok_generate("hi there")));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let body = json!({ "model": "gemini-3-flash", "messages": [{ "role": "user", "content": "hi" }], "max_tokens": 64 });
        let res = AntigravityAdapter
            .messages(&cx, &account, &body, &headers_with_raw_model("antigravity/gemini-3-flash"), &CallExtras::default())
            .await
            .unwrap();

        let sent = transport.requests()[0].json();
        assert_eq!(sent["request"]["contents"], json!([{ "role": "user", "parts": [{ "text": "hi" }] }]));
        assert_eq!(sent["request"]["generationConfig"]["maxOutputTokens"], json!(64));

        let json = body_json(res).await;
        assert_eq!(json["type"], json!("message"));
        assert_eq!(json["model"], json!("antigravity/gemini-3-flash"));
        assert_eq!(json["content"], json!([{ "type": "text", "text": "hi there" }]));
    }

    #[tokio::test]
    async fn rejects_a_garbage_reasoning_effort_with_a_400_before_calling_upstream() {
        let transport = MockTransport::new();
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let body = json!({ "model": "gemini-3-flash", "messages": [], "reasoning_effort": "turbo" });
        let res = AntigravityAdapter
            .messages(&cx, &account, &body, &HeaderMap::new(), &CallExtras::default())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn sends_a_namespaced_upstream_id_unchanged_instead_of_re_splitting_it() {
        let transport = MockTransport::new();
        transport.expect(|_| Ok(ok_generate("hello")));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        // The route already stripped the `antigravity/` provider prefix — a second split
        // would eat the first segment of a namespaced upstream id.
        let body = json!({ "model": "org/model-x", "messages": [{ "role": "user", "content": "hi" }], "max_tokens": 8 });
        AntigravityAdapter
            .messages(&cx, &account, &body, &HeaderMap::new(), &CallExtras::default())
            .await
            .unwrap();
        assert_eq!(transport.requests()[0].json()["model"], json!("org/model-x"));
    }

    // ── countTokens ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn posts_a_bare_request_body_and_answers_anthropic_shaped() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "totalTokens": 1234 }));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let body = json!({ "model": "gemini-3-flash", "messages": [{ "role": "user", "content": "hi" }] });
        let res = AntigravityAdapter
            .count_tokens(&cx, &account, &body, &HeaderMap::new(), &CallExtras::default())
            .await
            .unwrap();

        assert_eq!(transport.requests()[0].url, format!("{DAILY}/v1internal:countTokens"));
        let sent = transport.requests()[0].json();
        // countTokens' request proto only knows `request`: any generate-envelope field
        // (model, project, userAgent, requestType, requestId) or a request.sessionId 400s the
        // whole call (docs/providers.md § Antigravity).
        let keys: Vec<&String> = sent.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["request"]);
        assert!(sent["request"].get("sessionId").is_none());
        assert_eq!(body_json(res).await, json!({ "input_tokens": 1234 }));
    }

    #[tokio::test]
    async fn never_injects_the_claude_validated_tool_config_into_a_count() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "totalTokens": 99 }));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let body = json!({
            "model": "claude-opus-4-6-thinking",
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "name": "get_weather", "input_schema": { "type": "object", "properties": { "city": { "type": "string" } } } }],
        });
        AntigravityAdapter
            .count_tokens(&cx, &account, &body, &HeaderMap::new(), &CallExtras::default())
            .await
            .unwrap();
        let sent = transport.requests()[0].json();
        // VALIDATED is a generateContent-only rule; countTokens rejects it, which is what
        // made every Claude Code `/context` probe 400 in production.
        assert!(sent["request"].get("toolConfig").is_none());
        assert!(sent["request"].get("tools").is_some());
    }

    #[tokio::test]
    async fn count_tokens_rejects_an_invalid_reasoning_effort_before_calling_upstream() {
        let transport = MockTransport::new();
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let body = json!({ "model": "gemini-3-flash", "messages": [], "reasoning_effort": "turbo" });
        let res = AntigravityAdapter
            .count_tokens(&cx, &account, &body, &HeaderMap::new(), &CallExtras::default())
            .await
            .unwrap();
        // Same contract as `messages`: a malformed client field is the client's 400, never a
        // 502 for an upstream call that was never made.
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn rejects_a_200_without_a_usable_total_tokens_instead_of_fabricating_zero() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "totalTokens": "many" }));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let body = json!({ "model": "gemini-3-flash", "messages": [{ "role": "user", "content": "hi" }] });
        let res = AntigravityAdapter
            .count_tokens(&cx, &account, &body, &HeaderMap::new(), &CallExtras::default())
            .await
            .unwrap();
        // Clients budget context on this number — a malformed upstream payload must be a
        // detectable error, never a plausible `input_tokens: 0`.
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(body_json(res).await["type"], json!("error"));
    }

    // ── listModels ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn reads_the_live_fetch_available_models_map_ids_verbatim() {
        let transport = MockTransport::new();
        transport.respond_json(
            StatusCode::OK,
            json!({ "models": { "gemini-3.6-flash-high": { "displayName": "Gemini 3.6 Flash" }, "gemini-pro-agent": {} } }),
        );
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let result = AntigravityAdapter.list_models(&cx, &account).await;
        assert_eq!(transport.requests()[0].url, format!("{DAILY}/v1internal:fetchAvailableModels"));
        assert_eq!(transport.requests()[0].json(), json!({ "project": "proj-42" }));
        // The upstream id is what callers must send; the drifting UI name is only a label
        // (docs/providers.md § Antigravity).
        assert_eq!(
            result.models,
            vec![
                UpstreamModel { id: "gemini-3.6-flash-high".into(), display_name: Some("Gemini 3.6 Flash".into()) },
                UpstreamModel { id: "gemini-pro-agent".into(), display_name: None },
            ]
        );
        assert_eq!(result.error, None);
    }

    #[tokio::test]
    async fn returns_an_empty_list_with_the_error_rather_than_inventing_a_catalog() {
        let transport = MockTransport::new();
        for _ in 0..2 {
            transport.expect(|_| {
                Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new(), "nope"))
            });
        }
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let result = AntigravityAdapter.list_models(&cx, &account).await;
        assert!(result.models.is_empty());
        assert_eq!(result.error.as_deref(), Some("models fetch failed"));
    }

    #[tokio::test]
    async fn treats_an_array_shaped_models_payload_as_malformed() {
        let transport = MockTransport::new();
        for _ in 0..2 {
            transport.respond_json(StatusCode::OK, json!({ "models": [{ "id": "gemini-3-flash" }] }));
        }
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        // Publishing the entries of an array would invent "0", "1", … as callable model ids
        // and cache them for an hour.
        let result = AntigravityAdapter.list_models(&cx, &account).await;
        assert!(result.models.is_empty());
        assert_eq!(result.error.as_deref(), Some("models fetch failed"));
        assert_eq!(transport.requests().len(), 2);
    }

    #[tokio::test]
    async fn treats_a_well_formed_empty_models_map_as_a_real_answer() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "models": {} }));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        // An account with no currently available models must not probe the fallback host or
        // cache "models fetch failed" for an hour.
        let result = AntigravityAdapter.list_models(&cx, &account).await;
        assert!(result.models.is_empty());
        assert_eq!(result.error, None);
        assert_eq!(transport.requests().len(), 1);
    }

    #[tokio::test]
    async fn retries_the_project_bootstrap_for_an_account_that_has_none_stored() {
        let transport = MockTransport::new();
        transport.expect(|req| {
            assert!(req.url.contains("loadCodeAssist"));
            Ok(UpstreamResponse::json(
                StatusCode::OK,
                &json!({ "cloudaicompanionProject": "proj-new", "currentTier": { "id": "free-tier" } }),
            ))
        });
        transport.respond_json(StatusCode::OK, json!({ "models": { "gemini-3-flash": {} } }));
        let Some((cx, account)) = setup(transport.clone(), credential_without_project()).await else {
            return skip_without_db();
        };

        let result = AntigravityAdapter.list_models(&cx, &account).await;
        // The catalog is how a user discovers a callable model, so a login-time bootstrap
        // failure must not leave it permanently on "models fetch failed".
        let calls = transport.requests();
        assert!(calls[0].url.contains("loadCodeAssist"));
        let models_call = calls.iter().find(|c| c.url.contains("fetchAvailableModels")).expect("models call");
        assert_eq!(models_call.json(), json!({ "project": "proj-new" }));
        assert_eq!(result.models, vec![UpstreamModel { id: "gemini-3-flash".into(), display_name: None }]);
        assert_eq!(result.error, None);
    }

    // ── fetchUsage quota windows ────────────────────────────────────────────

    /// loadCodeAssist then retrieveUserQuotaSummary, in the order fetchUsage calls them.
    fn stub_quota(transport: &MockTransport, summary: Value, tier: Value) {
        let mut loaded = json!({ "cloudaicompanionProject": "proj-42" });
        if let Some(extra) = tier.as_object() {
            for (k, v) in extra {
                loaded[k] = v.clone();
            }
        }
        transport.expect(move |_| Ok(UpstreamResponse::json(StatusCode::OK, &loaded)));
        transport.expect(move |_| Ok(UpstreamResponse::json(StatusCode::OK, &summary)));
    }

    fn close_to(actual: Option<f64>, expected: f64) -> bool {
        actual.is_some_and(|a| (a - expected).abs() < 1e-5)
    }

    #[tokio::test]
    async fn compresses_the_labels_and_emits_5h_before_week() {
        let transport = MockTransport::new();
        stub_quota(
            &transport,
            json!({
                "groups": [
                    {
                        // Upstream lists Weekly first; the card wants the short window first.
                        "displayName": "GEMINI MODELS",
                        "buckets": [
                            { "bucketId": "gemini-weekly", "displayName": "Weekly Limit Remaining", "remainingFraction": 0.9998, "resetTime": "2026-08-29T09:00:00Z" },
                            { "bucketId": "gemini-5h", "displayName": "Five Hour Limit Remaining", "remainingFraction": 0.9987, "resetTime": "2026-08-22T14:00:00Z" },
                        ],
                    },
                    {
                        // Bundles two vendors, so it is named after neither.
                        "displayName": "CLAUDE AND GPT MODELS",
                        "buckets": [
                            { "displayName": "Weekly Limit Remaining", "remainingFraction": 1, "resetTime": null },
                            { "displayName": "Five Hour Limit Remaining", "remainingFraction": 1, "resetTime": null },
                        ],
                    },
                ]
            }),
            json!({}),
        );
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let usage = AntigravityAdapter.fetch_usage(&cx, &account).await;
        assert!(transport.requests().iter().any(|c| c.url.contains("retrieveUserQuotaSummary")));
        let labels: Vec<&str> = usage.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, vec!["Gemini 5h", "Gemini Week", "Other 5h", "Other Week"]);
        // utilization is percent *used*, so a 0.9998 remaining fraction is ~0.02.
        assert!(close_to(usage.windows[0].utilization, 0.13));
        assert_eq!(usage.windows[0].resets_at.as_deref(), Some("2026-08-22T14:00:00Z"));
        assert!(close_to(usage.windows[1].utilization, 0.02));
        assert_eq!(usage.windows[1].resets_at.as_deref(), Some("2026-08-29T09:00:00Z"));
        assert_eq!(usage.windows[2].utilization, Some(0.0));
        assert_eq!(usage.windows[2].resets_at, None);
        assert_eq!(usage.windows[3].utilization, Some(0.0));
        assert_eq!(usage.error, None);
    }

    #[tokio::test]
    async fn keeps_an_unrecognized_bucket_name_verbatim_after_the_two_known_windows() {
        let transport = MockTransport::new();
        stub_quota(
            &transport,
            json!({
                "groups": [{
                    "displayName": "GEMINI MODELS",
                    "buckets": [
                        { "displayName": "Monthly Allowance", "remainingFraction": 0.5 },
                        { "displayName": "Five Hour Limit Remaining", "remainingFraction": 0.5 },
                    ],
                }]
            }),
            json!({}),
        );
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let usage = AntigravityAdapter.fetch_usage(&cx, &account).await;
        let labels: Vec<&str> = usage.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, vec!["Gemini 5h", "Gemini Monthly Allowance"]);
    }

    #[tokio::test]
    async fn sends_the_stored_project_id_and_marks_an_exhausted_bucket_fully_used() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "cloudaicompanionProject": "proj-42" }));
        transport.respond_json(
            StatusCode::OK,
            json!({ "buckets": [{ "displayName": "Weekly", "remainingFraction": 0, "resetTime": "2026-09-01T00:00:00Z" }] }),
        );
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let usage = AntigravityAdapter.fetch_usage(&cx, &account).await;
        let quota_call = transport.requests().into_iter().find(|c| c.url.contains("retrieveUserQuotaSummary")).unwrap();
        assert_eq!(quota_call.json(), json!({ "project": "proj-42" }));
        // Ungrouped top-level buckets carry no prefix. 100 is what the routing module reads as
        // "unusable until resets_at".
        assert_eq!(
            usage.windows,
            vec![UsageWindow {
                label: "Week".into(),
                utilization: Some(100.0),
                resets_at: Some("2026-09-01T00:00:00Z".into()),
                value: None,
            }]
        );
    }

    #[tokio::test]
    async fn skips_disabled_buckets_and_leaves_an_amount_only_bucket_without_a_percentage() {
        let transport = MockTransport::new();
        stub_quota(
            &transport,
            json!({
                "groups": [{
                    "displayName": "GEMINI MODELS",
                    "buckets": [
                        { "displayName": "Retired", "remainingFraction": 0.5, "disabled": true },
                        { "displayName": "Credits", "remainingAmount": "250", "resetTime": null },
                    ],
                }]
            }),
            json!({}),
        );
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let usage = AntigravityAdapter.fetch_usage(&cx, &account).await;
        // A bare amount has no denominator, so no percentage is invented from it.
        assert_eq!(
            usage.windows,
            vec![UsageWindow { label: "Gemini Credits".into(), utilization: None, resets_at: None, value: None }]
        );
    }

    #[tokio::test]
    async fn keeps_the_account_metadata_and_reports_stale_when_only_the_quota_call_fails() {
        let transport = MockTransport::new();
        transport.respond_json(
            StatusCode::OK,
            json!({ "cloudaicompanionProject": "proj-42", "paidTier": { "id": "g1-pro-tier", "name": "Google AI Pro" } }),
        );
        transport
            .expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new(), "nope")));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let usage = AntigravityAdapter.fetch_usage(&cx, &account).await;
        assert!(usage.windows.is_empty());
        assert!(usage.stale);
        assert!(usage.error.as_deref().unwrap().contains("retrieveUserQuotaSummary 500"));
        // The cache merges this over the last good windows, so the tier must survive.
        assert_eq!(usage.account["plan_type"], json!("Google AI Pro"));
        assert_eq!(usage.account["project_id"], json!("proj-42"));
    }

    // ── fetchUsage metadata ─────────────────────────────────────────────────

    /// The `loadCodeAssist` answer plus a quota summary the test does not care about.
    fn stub_load_only(transport: &MockTransport, loaded: Value) {
        transport.expect(move |_| Ok(UpstreamResponse::json(StatusCode::OK, &loaded)));
        transport.respond_json(StatusCode::OK, json!({}));
    }

    #[tokio::test]
    async fn reports_the_tier_and_credit_balance_with_no_fabricated_usage_window() {
        let transport = MockTransport::new();
        stub_load_only(
            &transport,
            json!({
                "cloudaicompanionProject": "proj-42",
                "currentTier": { "id": "standard-tier" },
                "paidTier": {
                    "id": "ai-pro",
                    "availableCredits": [{ "creditType": "GOOGLE_ONE_AI", "creditAmount": "1500", "minimumCreditAmountForUsage": "10" }],
                },
            }),
        );
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };

        let usage = AntigravityAdapter.fetch_usage(&cx, &account).await;
        // Antigravity publishes no percentage quota here, so no window is invented.
        assert!(usage.windows.is_empty());
        assert_eq!(usage.account["plan_type"], json!("ai-pro"));
        assert_eq!(usage.account["project_id"], json!("proj-42"));
        assert_eq!(usage.account["credits_remaining"], json!(1500));
        // The usage floor is deliberately not surfaced: it would be a gate on credits whose
        // semantics are unverified (docs/providers.md § Antigravity).
        assert!(!usage.account.contains_key("credits_minimum"));
        assert_eq!(usage.error, None);
    }

    #[tokio::test]
    async fn keeps_a_zero_balance_as_a_fact_rather_than_dropping_it_as_falsy() {
        let transport = MockTransport::new();
        stub_load_only(
            &transport,
            json!({
                "cloudaicompanionProject": "proj-42",
                "paidTier": { "id": "ai-pro", "availableCredits": [{ "creditType": "GOOGLE_ONE_AI", "creditAmount": "0" }] },
            }),
        );
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let usage = AntigravityAdapter.fetch_usage(&cx, &account).await;
        assert_eq!(usage.account["credits_remaining"], json!(0));
    }

    #[tokio::test]
    async fn omits_the_balance_when_the_matching_entry_reports_only_a_usage_floor() {
        // The shape Google actually returns for a Google AI Pro account (measured
        // 2026-08-22): the GOOGLE_ONE_AI entry matches, but there is no creditAmount to read
        // — see docs/providers.md § Antigravity.
        let transport = MockTransport::new();
        stub_load_only(
            &transport,
            json!({
                "cloudaicompanionProject": "proj-42",
                "paidTier": {
                    "id": "g1-pro-tier",
                    "name": "Google AI Pro",
                    "availableCredits": [{ "creditType": "GOOGLE_ONE_AI", "minimumCreditAmountForUsage": "50" }],
                },
            }),
        );
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let usage = AntigravityAdapter.fetch_usage(&cx, &account).await;
        assert!(!usage.account.contains_key("credits_remaining"));
        // The plan's own name beats title-casing "g1-pro-tier" ourselves.
        assert_eq!(usage.account["plan_type"], json!("Google AI Pro"));
        // A floor with no balance is not a failed read.
        assert_eq!(usage.error, None);
        assert!(!usage.stale);
    }

    #[tokio::test]
    async fn falls_back_to_the_paid_tier_id_when_the_response_carries_no_plan_name() {
        let transport = MockTransport::new();
        stub_load_only(
            &transport,
            json!({ "cloudaicompanionProject": "proj-42", "paidTier": { "id": "g1-ultra-tier" } }),
        );
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let usage = AntigravityAdapter.fetch_usage(&cx, &account).await;
        assert_eq!(usage.account["plan_type"], json!("g1-ultra-tier"));
    }

    #[tokio::test]
    async fn omits_the_balance_for_a_tier_with_no_google_one_ai_credit_entry() {
        let transport = MockTransport::new();
        stub_load_only(
            &transport,
            json!({ "cloudaicompanionProject": "proj-42", "currentTier": { "id": "free-tier" } }),
        );
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let usage = AntigravityAdapter.fetch_usage(&cx, &account).await;
        // The row says the balance is unavailable rather than printing a zero the upstream
        // never reported.
        assert!(!usage.account.contains_key("credits_remaining"));
        assert_eq!(usage.account["plan_type"], json!("free-tier"));
    }

    #[tokio::test]
    async fn marks_the_snapshot_stale_on_failure_rather_than_blanking_it() {
        let transport = MockTransport::new();
        transport.expect(|_| {
            Ok(UpstreamResponse::from_bytes(StatusCode::SERVICE_UNAVAILABLE, HeaderMap::new(), "boom"))
        });
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let usage = AntigravityAdapter.fetch_usage(&cx, &account).await;
        assert!(usage.stale);
        assert!(usage.error.as_deref().unwrap().contains("loadCodeAssist 503"));
    }

    // ── Antigravity client identity ─────────────────────────────────────────

    #[tokio::test]
    async fn sends_the_cli_user_agent_on_the_anthropic_surface_too() {
        let transport = MockTransport::new();
        transport.expect(|_| Ok(ok_generate("hi there")));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let body = json!({ "model": "gemini-3-flash", "messages": [{ "role": "user", "content": "hi" }], "max_tokens": 64 });
        AntigravityAdapter
            .messages(&cx, &account, &body, &headers_with_raw_model("antigravity/gemini-3-flash"), &CallExtras::default())
            .await
            .unwrap();
        assert_eq!(transport.requests()[0].header("user-agent"), Some(CLI_UA));
    }

    #[tokio::test]
    async fn sends_the_cli_user_agent_on_fetch_available_models() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "models": {} }));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        AntigravityAdapter.list_models(&cx, &account).await;
        assert_eq!(transport.requests()[0].url, format!("{DAILY}/v1internal:fetchAvailableModels"));
        assert_eq!(transport.requests()[0].header("user-agent"), Some(CLI_UA));
    }

    #[tokio::test]
    async fn keeps_the_hub_user_agent_on_the_onboard_user_control_plane_call() {
        let transport = MockTransport::new();
        transport
            .respond_json(StatusCode::OK, json!({ "done": true, "response": { "cloudaicompanionProject": "proj-42" } }));
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool, transport.clone());

        let project = onboard_user(&cx, "at-1", "free-tier").await.unwrap();
        assert_eq!(project, "proj-42");

        let calls = transport.requests();
        assert_eq!(calls[0].header("user-agent"), Some("antigravity/hub/2.2.1 darwin/arm64 google-api-nodejs-client/10.3.0"));
        assert_eq!(calls[0].header("x-goog-api-client"), Some("gl-node/22.21.1"));
        // The metadata version tracks the hub pin, not the CLI one.
        assert_eq!(calls[0].json()["metadata"]["ide_version"], json!("2.2.1"));
    }

    #[tokio::test]
    async fn honours_the_hub_version_override_without_touching_the_cli_ua() {
        let transport = MockTransport::new();
        transport
            .respond_json(StatusCode::OK, json!({ "done": true, "response": { "cloudaicompanionProject": "proj-42" } }));
        transport.expect(|_| Ok(ok_generate("hello")));
        let Some((cx, account)) = setup(transport.clone(), credential()).await else { return skip_without_db() };
        let mut config = cx.config().clone();
        config.antigravity_hub_version = Some("3.0.0".into());
        let cx = AppState::builder(config, cx.pool().clone()).transport(transport.clone()).build();

        onboard_user(&cx, "at-1", "free-tier").await.unwrap();
        assert_eq!(
            transport.requests()[0].header("user-agent"),
            Some("antigravity/hub/3.0.0 darwin/arm64 google-api-nodejs-client/10.3.0")
        );

        AntigravityAdapter.chat_completions(&cx, &account, &chat_request(), &CallExtras::default()).await.unwrap();
        assert_eq!(transport.requests()[1].header("user-agent"), Some(CLI_UA));
    }
}
