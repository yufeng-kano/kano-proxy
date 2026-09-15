//! Port of apps/api/src/routes/providers.ts — `/api/providers`, the session-authenticated
//! surface behind the admin Providers page (docs/admin-ui.md § Providers page) and the
//! provider login flows (docs/auth.md § Provider accounts).
//!
//! Three things live here and nowhere else: the account list with its computed status dot,
//! the OAuth login start/complete handlers for the four builtin providers, and the pool's
//! routing-strategy write. Every upstream call goes through `cx.transport()`; a decrypted
//! credential never leaves this module.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::auth::pkce::parse_code_hash_state;
use crate::auth::provider_oauth::{
    antigravity_oauth_client, begin_antigravity_authorization, begin_claude_authorization, exchange_antigravity_code,
    exchange_claude_code, extract_chatgpt_account_id, extract_jwt_expiry_iso, parse_antigravity_callback,
    AntigravityOAuth, CodexDeviceAuth, OAuthTokens, PendingAntigravityOAuth, PendingOAuth,
};
use crate::auth::session::SessionUser;
use crate::crypto::token_crypto::encrypt_json;
use crate::db::accounts::{
    acquire_usage_lock, get_account, insert_account, is_usage_fresh, iso_from_ms, list_accounts, parse_iso_ms,
    promote_account, read_usage_snapshot, remove_account, set_account_custom_label, AccountRow, NewAccount,
};
use crate::db::oauth_states::{
    delete_expired_states, delete_state, get_provider_state, insert_provider_state, list_provider_states,
    OAuthLoginStateRow, LOGIN_STATE_TTL_MS,
};
use crate::db::provider_settings::{get_provider_strategy, set_provider_strategy};
use crate::ids::{new_id, now_iso};
use crate::pool::bench::{bench_until_from_row, clear_bench};
use crate::pool::extension::ListSharedOptions;
use crate::pool::{SharedAccount, StoredCredential};
use crate::providers::antigravity::bootstrap_antigravity_project;
use crate::providers::claude_code::FABLE_MODEL_PREFIX;
use crate::providers::codex_usage::{fetch_codex_usage_json, usage_payload_value};
use crate::providers::identity::{
    codex_identity, fetch_antigravity_identity, fetch_claude_identity, fetch_grok_identity, pick_account_label,
};
use crate::providers::registry::get_adapter;
use crate::providers::types::{DynAdapter, UsageWindow};
use crate::providers::usage_refresh::fetch_and_persist_usage;
use crate::providers::ProviderId;
use crate::routing::facts::{usage_window_unusable_until, windows_unusable_until};
use crate::routing::strategy::DEFAULT_STRATEGY;
use crate::upstream::UpstreamRequest;
use crate::AppState;

/// Grok's public device-flow client, used when `GROK_OAUTH_CLIENT_ID` is unset.
const GROK_DEFAULT_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const GROK_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const GROK_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const GROK_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const CODEX_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/{provider}", patch(set_strategy))
        .route("/{provider}/accounts", get(list_accounts_route))
        .route("/{provider}/accounts/import", post(import_account))
        .route("/{provider}/accounts/{id}", patch(patch_account).delete(delete_account))
        .route("/{provider}/accounts/{id}/promote", post(promote))
        .route("/{provider}/accounts/{id}/unpause", post(unpause))
        .route("/{provider}/login", post(login))
        .route("/{provider}/login/{id}/complete", post(login_complete))
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn internal_error() -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
}

fn invalid_provider() -> Response {
    error(StatusCode::BAD_REQUEST, "invalid provider")
}

/// The request body as JSON, or `None` when it is absent or unparsable — the `try { await
/// c.req.json() } catch` the TypeScript wrote around every one of these handlers.
fn parse_body(bytes: &Bytes) -> Option<Value> {
    serde_json::from_slice(bytes).ok()
}

/// A body that is absent or unparsable reads as `{}`, as `c.req.json().catch(() => ({}))` did.
fn object_body(bytes: &Bytes) -> Map<String, Value> {
    match parse_body(bytes) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// The rows other users share with this viewer for one builtin provider
/// (docs/cloud-edition.md § "Pool extension"). Always `[]` on a standalone install — no
/// extension, no cross-user lookup anywhere in the core.
async fn shared_accounts(
    cx: &AppState,
    user_id: &str,
    provider: Option<ProviderId>,
    options: ListSharedOptions,
) -> Vec<SharedAccount> {
    let (Some(ext), Some(provider)) = (cx.pool_extension(), provider) else { return Vec::new() };
    ext.list_shared(cx, user_id, provider, options).await.unwrap_or_default()
}

/// One row the viewer borrows rather than owns — mutating it is the owner's right alone (403),
/// except promote.
async fn find_shared(cx: &AppState, user_id: &str, provider: Option<ProviderId>, account_id: &str) -> bool {
    shared_accounts(cx, user_id, provider, ListSharedOptions::default())
        .await
        .iter()
        .any(|s| s.account.id == account_id)
}

// --------------------------------------------------------------- GET /:provider/accounts

#[derive(Debug, Deserialize, Default)]
struct AccountsQuery {
    refresh: Option<String>,
}

/// One row of `GET /:provider/accounts`, before the status pass rewrites `status`.
struct AccountView {
    id: String,
    priority: i32,
    status: &'static str,
    label: String,
    custom_label: Option<String>,
    account: Option<Map<String, Value>>,
    usage_windows: Option<Vec<Value>>,
    error: Option<String>,
    stale: bool,
    share: Option<crate::pool::ShareInfo>,
}

impl AccountView {
    fn to_json(&self) -> Value {
        let mut out = json!({
            "id": self.id,
            "priority": self.priority,
            "status": self.status,
            "label": self.label,
            "custom_label": self.custom_label,
            "account": self.account,
            "usage": self.usage_windows.as_ref().map(|w| json!({ "windows": w })),
            "error": self.error,
            "stale": self.stale,
        });
        if let Some(share) = &self.share {
            out["share"] = json!({
                "teamId": share.team_id,
                "teamName": share.team_name,
                "ownerLabel": share.owner_label,
            });
        }
        out
    }
}

/// `401|invalid token|unauthorized` in the stored error text, the signal the TypeScript's
/// `/401|invalid.?token|unauthorized/i` regex looked for.
fn looks_unauthorized(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    if lower.contains("401") || lower.contains("unauthorized") {
        return true;
    }
    // `invalid.?token`: "invalid token", "invalid_token", "invalidtoken".
    let bytes = lower.as_bytes();
    let needle = b"invalid";
    for start in 0..bytes.len() {
        if !bytes[start..].starts_with(needle) {
            continue;
        }
        let rest = &lower[start + needle.len()..];
        if rest.starts_with("token") || (rest.len() > 1 && rest[1..].starts_with("token")) {
            return true;
        }
    }
    false
}

/// The display label precedence the Providers page shows: the user's own name first, then the
/// upstream identity, then whatever the row was bound with.
fn display_label(row: &AccountRow, meta: Option<&Map<String, Value>>) -> String {
    let from_meta = |key: &str| meta.and_then(|m| m.get(key)).and_then(Value::as_str).filter(|s| !s.is_empty());
    row.custom_label
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| from_meta("email"))
        .or_else(|| from_meta("display_name"))
        .or_else(|| from_meta("username"))
        .or_else(|| row.label.as_deref().filter(|s| !s.is_empty()))
        .unwrap_or(&row.id)
        .to_string()
}

fn merged_meta(base: Option<Map<String, Value>>, extra: Option<&Map<String, Value>>) -> Option<Map<String, Value>> {
    let Some(extra) = extra else { return base };
    let mut out = base.unwrap_or_default();
    for (k, v) in extra {
        out.insert(k.clone(), v.clone());
    }
    Some(out)
}

/// Usage for one of the viewer's own rows: cache hit, single-flight fetch, or the stored
/// snapshot when another caller holds the lock (docs/providers.md § Usage cache).
async fn account_usage(
    cx: &AppState,
    row: &AccountRow,
    adapter: &DynAdapter,
    force: bool,
    view: &mut AccountView,
) {
    if !adapter.has_fetch_usage() {
        return;
    }
    let fresh = !force && is_usage_fresh(row, cx.now_ms());
    let mut cached = if fresh { read_usage_snapshot(row) } else { None };
    let lock_token = match cached {
        Some(_) => None,
        None => acquire_usage_lock(cx.pool(), &row.id).await.ok().flatten(),
    };
    // Lost the lock: another request is already calling upstream, so serve whatever snapshot
    // exists rather than queueing behind it.
    if cached.is_none() && lock_token.is_none() {
        cached = read_usage_snapshot(row);
    }

    let mark_unusable = |view: &mut AccountView, error: Option<&str>, windows: &[Value], edge_blocked: bool| {
        if view.status != "benched"
            && windows.is_empty()
            && !edge_blocked
            && error.is_some_and(looks_unauthorized)
        {
            view.status = "unusable";
        }
    };

    if let Some(cached) = cached {
        view.usage_windows = Some(cached.windows.clone());
        view.account = merged_meta(view.account.take(), cached.account.as_ref());
        // Anything served past its TTL is flagged stale, even though the in-flight fetch that
        // beat us here will refresh it shortly.
        view.stale = cached.stale || !fresh;
        view.error = cached.error.clone();
        mark_unusable(view, view.error.clone().as_deref(), &cached.windows, cached.edge_blocked);
        return;
    }
    let Some(lock_token) = lock_token else { return };

    // Shared core with the routing module's background refresh — the lock/fetch/write sequence
    // itself lives in `usage_refresh`, never duplicated here.
    match fetch_and_persist_usage(cx, row, adapter.as_ref(), &lock_token).await {
        Ok(snap) => {
            view.usage_windows = Some(snap.windows.clone());
            view.account = merged_meta(view.account.take(), snap.account.as_ref());
            view.stale = snap.stale;
            view.error = snap.error.clone();
            // A usage 403 bot-wall must NOT mark the account unusable — chat can still work.
            mark_unusable(view, snap.error.as_deref(), &snap.windows, snap.edge_blocked);
        }
        Err(error) => {
            view.error = Some(error.clone());
            view.stale = true;
            // Serve the unchanged snapshot too: one upstream hiccup must not blank the usage
            // bars for the request that encountered it.
            if let Some(prior) = read_usage_snapshot(row) {
                view.usage_windows = Some(prior.windows.clone());
                view.account = merged_meta(view.account.take(), prior.account.as_ref());
                mark_unusable(view, Some(error.as_str()), &prior.windows, prior.edge_blocked);
            }
        }
    }
}

async fn list_accounts_route(
    State(state): State<AppState>,
    session: SessionUser,
    Path(provider): Path<String>,
    Query(query): Query<AccountsQuery>,
) -> Response {
    let Some(provider) = ProviderId::parse(&provider) else { return invalid_provider() };
    let user_id = &session.user.id;

    // Usage is cached 2 min server-side behind a single-flight lock, so N devices cost one
    // upstream call, not N (docs/providers.md § Usage cache).
    let force = query.refresh.as_deref() == Some("true");
    let Ok(rows) = list_accounts(state.pool(), user_id, provider.as_str()).await else {
        return internal_error();
    };
    let adapter = get_adapter(provider);
    let now = state.now_ms();

    let mut accounts: Vec<AccountView> = Vec::with_capacity(rows.len());
    // Parallel to `accounts`: the routing fact behind a "limited" dot, computed from the
    // snapshot each row actually returns rather than re-read off the row.
    let mut limited_until: Vec<Option<i64>> = Vec::new();
    // Parallel too: the row's `created_at`, needed to walk the rows in the router's merged
    // order below when shared rows are in play.
    let mut created_at: Vec<String> = Vec::new();
    // Parallel too: whether the row's seat can serve Fable at all, from the same merged
    // profile facts the router reads. Always true for adapters without a rule.
    let mut fable_eligible: Vec<bool> = Vec::new();

    for row in &rows {
        let benched = bench_until_from_row(row, now).is_some();
        let meta = row
            .account_meta_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|v| v.as_object().cloned());
        let mut view = AccountView {
            id: row.id.clone(),
            priority: row.priority,
            // Provisional only — the pass after this loop assigns active / standby / limited
            // from the routing facts, in list order.
            status: if benched { "benched" } else { "standby" },
            label: String::new(),
            custom_label: row.custom_label.clone(),
            account: meta,
            usage_windows: None,
            error: None,
            stale: false,
            share: None,
        };
        account_usage(&state, row, &adapter, force, &mut view).await;
        view.label = display_label(row, view.account.as_ref());

        limited_until.push(view.usage_windows.as_ref().and_then(|w| windows_unusable_until(w, now)));
        created_at.push(row.created_at.clone());
        fable_eligible.push(adapter.supports_model(view.account.as_ref(), FABLE_MODEL_PREFIX));
        accounts.push(view);
    }

    // The edition's own bars on the viewer's own rows (an allowance on an account they lend
    // out), placed before the upstream windows. Added after `limited_until` was read so they
    // never colour the routing dot.
    if let Some(ext) = state.pool_extension() {
        if !accounts.is_empty() {
            let ids: Vec<String> = accounts.iter().map(|a| a.id.clone()).collect();
            let extra: HashMap<String, Vec<UsageWindow>> =
                ext.own_bars(&state, user_id, provider, &ids).await.unwrap_or_default();
            for account in &mut accounts {
                let Some(bars) = extra.get(&account.id).filter(|b| !b.is_empty()) else { continue };
                let mut windows: Vec<Value> =
                    bars.iter().map(|w| serde_json::to_value(w).expect("usage window serializes")).collect();
                windows.extend(account.usage_windows.take().unwrap_or_default());
                account.usage_windows = Some(windows);
            }
        }
    }

    // Rows shared with the viewer, collected after the viewer's own pool and merged into
    // routing order below. **No usage surface**: windows, probe errors and the upstream
    // identity blob belong to the owner's page, not the borrower's, and nothing here ever
    // triggers an upstream usage call for someone else's account.
    for shared in shared_accounts(&state, user_id, Some(provider), ListSharedOptions { usage: true }).await {
        let row = &shared.account;
        accounts.push(AccountView {
            id: row.id.clone(),
            priority: shared.priority,
            status: if bench_until_from_row(row, now).is_some() { "benched" } else { "standby" },
            label: row
                .custom_label
                .as_deref()
                .filter(|s| !s.is_empty())
                .or_else(|| row.label.as_deref().filter(|s| !s.is_empty()))
                .unwrap_or(&row.id)
                .to_string(),
            custom_label: row.custom_label.clone(),
            account: None,
            // The extension's own bars for the borrower (an allowance), never the owner's
            // upstream windows.
            usage_windows: shared.usage.as_ref().map(|u| {
                u.windows.iter().map(|w| serde_json::to_value(w).expect("usage window serializes")).collect()
            }),
            error: None,
            stale: false,
            share: Some(shared.share.clone()),
        });
        limited_until.push(usage_window_unusable_until(row, now));
        created_at.push(row.created_at.clone());
        fable_eligible.push(adapter.supports_model(None, FABLE_MODEL_PREFIX));
    }

    // The dot says where a request goes right now, so it is the router's own walk over the
    // same stored facts: an exhausted usage window is "limited", not active, and "active"
    // lands on the first account the ordered walk would actually pick. A Claude Code pool
    // carries the Fable route on the same dot (docs/admin-ui.md § Providers page). Shared rows
    // are appended to the list, but the router merges them into the pool by
    // `(priority DESC, created_at DESC)`, so the dot has to be assigned in that order.
    let mut route_order: Vec<usize> = (0..accounts.len()).collect();
    route_order.sort_by(|&x, &y| {
        accounts[y].priority.cmp(&accounts[x].priority).then_with(|| created_at[y].cmp(&created_at[x]))
    });
    let mut routed = false;
    let mut fable_routed = false;
    for &i in &route_order {
        if accounts[i].status == "benched" || accounts[i].status == "unusable" {
            continue;
        }
        if limited_until[i].is_some() {
            accounts[i].status = "limited";
            continue;
        }
        let fable = fable_eligible[i];
        if !routed {
            routed = true;
            fable_routed = fable;
            accounts[i].status = if fable { "active" } else { "active_no_fable" };
            continue;
        }
        if fable && !fable_routed {
            fable_routed = true;
            accounts[i].status = "active_fable";
            continue;
        }
        accounts[i].status = "standby";
    }

    // The pool's routing strategy (docs/providers.md § Routing module) — no separate read
    // route; `PATCH /api/providers/:provider` is the only writer.
    let Ok(strategy) = get_provider_strategy(state.pool(), user_id, provider.as_str()).await else {
        return internal_error();
    };

    // The list is returned in that same merged order, so the page's list-position assumptions
    // (first row is Primary) match where traffic goes.
    let ordered: Vec<Value> = route_order.iter().map(|&i| accounts[i].to_json()).collect();
    Json(json!({ "available": true, "accounts": ordered, "models": [], "error": null, "strategy": strategy }))
        .into_response()
}

// ---------------------------------------------------------------- PATCH /:provider

/// Sets the pool's routing strategy (docs/providers.md § Routing module): body `{strategy}`,
/// only `ordered` accepted today. Upserts `provider_settings` — a missing row means `ordered`,
/// so this is the only writer of that table.
async fn set_strategy(
    State(state): State<AppState>,
    session: SessionUser,
    Path(provider): Path<String>,
    body: Bytes,
) -> Response {
    let Some(provider) = ProviderId::parse(&provider) else { return invalid_provider() };
    let Some(body) = parse_body(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid strategy");
    };
    let raw = body.get("strategy").and_then(Value::as_str);
    if raw != Some(DEFAULT_STRATEGY) {
        return error(StatusCode::BAD_REQUEST, &format!("strategy must be \"{DEFAULT_STRATEGY}\""));
    }
    if set_provider_strategy(state.pool(), &session.user.id, provider.as_str(), DEFAULT_STRATEGY).await.is_err() {
        return internal_error();
    }
    Json(json!({ "ok": true, "strategy": DEFAULT_STRATEGY })).into_response()
}

// ------------------------------------------------------- PATCH /:provider/accounts/:id

async fn patch_account(
    State(state): State<AppState>,
    session: SessionUser,
    Path((provider, id)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let Some(body) = parse_body(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid custom_label");
    };
    let custom_label = match body.get("custom_label") {
        Some(Value::Null) | None => None,
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else if trimmed.chars().count() > 64 {
                return error(StatusCode::BAD_REQUEST, "custom_label too long");
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(_) => return error(StatusCode::BAD_REQUEST, "invalid custom_label"),
    };

    match set_account_custom_label(state.pool(), &session.user.id, &id, custom_label.as_deref()).await {
        Err(_) => internal_error(),
        Ok(true) => Json(json!({ "ok": true, "custom_label": custom_label })).into_response(),
        // A row the viewer borrows exists, but renaming it is the owner's right.
        Ok(false) => forbidden_or_not_found(&state, &session.user.id, &provider, &id).await,
    }
}

async fn forbidden_or_not_found(state: &AppState, user_id: &str, provider: &str, id: &str) -> Response {
    if find_shared(state, user_id, ProviderId::parse(provider), id).await {
        return error(StatusCode::FORBIDDEN, "forbidden");
    }
    error(StatusCode::NOT_FOUND, "not found")
}

// ------------------------------------------------ POST /:provider/accounts/:id/promote

/// Promote is the one mutation a borrower may make on a shared row: it reorders that row
/// inside **the viewer's own** merged pool, never the owner's (docs/cloud-edition.md § "Pool
/// extension"), so it delegates to `set_shared_priority` with one above the current maximum
/// across both kinds.
async fn promote(
    State(state): State<AppState>,
    session: SessionUser,
    Path((provider, id)): Path<(String, String)>,
) -> Response {
    let user_id = &session.user.id;
    match promote_account(state.pool(), user_id, &id).await {
        Err(_) => return internal_error(),
        Ok(true) => return Json(json!({ "ok": true })).into_response(),
        Ok(false) => {}
    }

    let provider = ProviderId::parse(&provider);
    let shared = shared_accounts(&state, user_id, provider, ListSharedOptions::default()).await;
    if !shared.iter().any(|s| s.account.id == id) {
        return error(StatusCode::NOT_FOUND, "not found");
    }
    let Some(provider) = provider else { return error(StatusCode::NOT_FOUND, "not found") };
    let Ok(own) = list_accounts(state.pool(), user_id, provider.as_str()).await else {
        return internal_error();
    };
    let max = own
        .iter()
        .map(|a| a.priority)
        .chain(shared.iter().map(|s| s.priority))
        .fold(0, i32::max);
    let Some(ext) = state.pool_extension() else { return error(StatusCode::NOT_FOUND, "not found") };
    match ext.set_shared_priority(&state, user_id, &id, max + 1).await {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, "not found"),
        Err(_) => internal_error(),
    }
}

// ------------------------------------------------ POST /:provider/accounts/:id/unpause

async fn unpause(
    State(state): State<AppState>,
    session: SessionUser,
    Path((provider, id)): Path<(String, String)>,
) -> Response {
    let Some(provider) = ProviderId::parse(&provider) else { return invalid_provider() };
    let user_id = &session.user.id;
    let row = match get_account(state.pool(), user_id, &id).await {
        Ok(row) => row,
        Err(_) => return internal_error(),
    };
    let Some(row) = row.filter(|r| r.provider == provider.as_str()) else {
        // Unpausing someone else's account is the owner's call, not a borrower's.
        return forbidden_or_not_found(&state, user_id, provider.as_str(), &id).await;
    };
    if clear_bench(state.pool(), user_id, provider.as_str(), &row.id).await.is_err() {
        return internal_error();
    }
    Json(json!({ "ok": true })).into_response()
}

// ------------------------------------------------------ DELETE /:provider/accounts/:id

async fn delete_account(
    State(state): State<AppState>,
    session: SessionUser,
    Path((provider, id)): Path<(String, String)>,
) -> Response {
    match remove_account(state.pool(), &session.user.id, &id).await {
        Err(_) => internal_error(),
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        // Only the owner can unbind an account the viewer merely borrows.
        Ok(false) => forbidden_or_not_found(&state, &session.user.id, &provider, &id).await,
    }
}

// ------------------------------------------------- POST /:provider/accounts/import

/// Manual credential ingest for bootstrapping / tests (session required).
async fn import_account(
    State(state): State<AppState>,
    session: SessionUser,
    Path(provider): Path<String>,
    body: Bytes,
) -> Response {
    let Some(provider) = ProviderId::parse(&provider) else { return invalid_provider() };
    if state.config().token_encryption_key.is_none() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "TOKEN_ENCRYPTION_KEY not configured");
    }
    let body = object_body(&body);
    let text = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_string);
    let Some(access_token) = text("access_token").filter(|s| !s.is_empty()) else {
        return error(StatusCode::BAD_REQUEST, "access_token required");
    };
    let email = text("email");
    let account_id = text("account_id");
    let credential = StoredCredential {
        access_token,
        refresh_token: text("refresh_token"),
        expires_at: text("expires_at"),
        account_id: account_id.clone(),
        email: email.clone(),
        client_id: text("client_id"),
        token_endpoint: text("token_endpoint"),
        extra: None,
    };
    let Ok(encrypted) = encrypt_json(state.config().token_encryption_key.as_deref(), &credential) else {
        return internal_error();
    };
    let meta = json!({ "email": email }).to_string();
    let label = text("label").or(email);
    match insert_account(
        state.pool(),
        NewAccount {
            user_id: &session.user.id,
            provider: provider.as_str(),
            encrypted_payload: &encrypted,
            label: label.as_deref(),
            external_account_id: account_id.as_deref(),
            account_meta_json: Some(&meta),
            priority: None,
        },
    )
    .await
    {
        Ok(row) => Json(json!({ "ok": true, "id": row.id })).into_response(),
        Err(_) => internal_error(),
    }
}

// ------------------------------------------------------------- POST /:provider/login

fn login_expiry(state: &AppState) -> String {
    iso_from_ms(state.now_ms() + LOGIN_STATE_TTL_MS)
}

async fn login(State(state): State<AppState>, session: SessionUser, Path(provider): Path<String>) -> Response {
    let Some(provider) = ProviderId::parse(&provider) else { return invalid_provider() };
    let login_id = new_id("login");
    let expires = login_expiry(&state);

    // Also opportunistically pruned here on the same path that adds new rows, ahead of
    // whatever the next daily retention sweep does (docs/logging.md).
    let _ = delete_expired_states(state.pool(), &now_iso()).await;

    match provider {
        ProviderId::ClaudeCode => claude_login(&state, &session, provider, &login_id, &expires).await,
        ProviderId::Codex => codex_login(&state, &session, provider, &login_id, &expires).await,
        ProviderId::Antigravity => antigravity_login(&state, &session, provider, &login_id, &expires).await,
        ProviderId::Grok => grok_login(&state, &session, provider, &login_id, &expires).await,
    }
}

async fn store_pending(
    state: &AppState,
    session: &SessionUser,
    provider: ProviderId,
    login_id: &str,
    payload: &Value,
    expires: &str,
) -> Option<Response> {
    insert_provider_state(state.pool(), login_id, &session.user.id, provider.as_str(), &payload.to_string(), expires)
        .await
        .err()
        .map(|_| internal_error())
}

async fn claude_login(
    state: &AppState,
    session: &SessionUser,
    provider: ProviderId,
    login_id: &str,
    expires: &str,
) -> Response {
    let begun = begin_claude_authorization(state.config().claude_code_oauth_client_id.as_deref());
    let payload = serde_json::to_value(&begun.pending).expect("pending serializes");
    if let Some(failed) = store_pending(state, session, provider, login_id, &payload, expires).await {
        return failed;
    }
    Json(json!({
        "login_id": login_id,
        "authorization_url": begun.authorization_url,
        "instructions": "Open authorization_url, approve, then paste code#state from the Anthropic callback page.",
    }))
    .into_response()
}

async fn codex_login(
    state: &AppState,
    session: &SessionUser,
    provider: ProviderId,
    login_id: &str,
    expires: &str,
) -> Response {
    let client_id = state
        .config()
        .codex_oauth_client_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(CodexDeviceAuth::CLIENT_ID)
        .to_string();
    // A form body is rejected by OpenAI with model_attributes_type, so this is JSON.
    let request = UpstreamRequest::post(CodexDeviceAuth::USER_CODE_URL)
        .header("accept", "application/json")
        .json(&json!({ "client_id": client_id }));
    let response = match state.transport().send(request).await {
        Ok(response) => response,
        Err(e) => return error(StatusCode::BAD_GATEWAY, &format!("device code failed: {e}")),
    };
    if !response.status.is_success() {
        let status = response.status.as_u16();
        let detail = response.text().await.unwrap_or_default().trim().to_string();
        let detail = if detail.is_empty() { format!("HTTP {status}") } else { detail };
        return error(StatusCode::BAD_GATEWAY, &format!("device code failed: {detail}"));
    }
    let device = response.json_value().await.unwrap_or(Value::Null);
    let Some(device_auth_id) = device.get("device_auth_id").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return error(StatusCode::BAD_GATEWAY, "device code response missing device_auth_id");
    };
    let Some(user_code) = device.get("user_code").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return error(StatusCode::BAD_GATEWAY, "device code response missing user_code");
    };
    // `interval` arrives as a number or a numeric string; anything else falls back to 5.
    let interval = match device.get("interval") {
        Some(Value::Number(n)) => n.as_f64().filter(|v| v.is_finite()).unwrap_or(5.0),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(5.0),
        _ => 5.0,
    };
    // JavaScript has one number type, so a whole interval went out as `7`, not `7.0`.
    let interval =
        if interval.fract() == 0.0 { Value::from(interval as i64) } else { Value::from(interval) };

    let payload = json!({ "client_id": client_id, "device_auth_id": device_auth_id, "user_code": user_code });
    if let Some(failed) = store_pending(state, session, provider, login_id, &payload, expires).await {
        return failed;
    }
    Json(json!({
        "login_id": login_id,
        "user_code": user_code,
        "verification_uri": CODEX_VERIFICATION_URI,
        "interval": interval,
    }))
    .into_response()
}

async fn antigravity_login(
    state: &AppState,
    session: &SessionUser,
    provider: ProviderId,
    login_id: &str,
    expires: &str,
) -> Response {
    // Unlike the other three, this provider ships with no built-in client: it needs an OAuth
    // client *secret*, which is never committed here (docs/auth.md § Antigravity). Refuse
    // plainly instead of starting a flow that cannot finish.
    let Some((client_id, _)) = antigravity_oauth_client(state) else {
        return error(
            StatusCode::BAD_REQUEST,
            "Antigravity is not configured on this deploy: set ANTIGRAVITY_OAUTH_CLIENT_ID and ANTIGRAVITY_OAUTH_CLIENT_SECRET.",
        );
    };
    // Google only accepts this client's own registered localhost redirect, so the proxy cannot
    // host a callback: the user approves, lands on a page that does not load, and pastes the
    // URL (or the bare code) back.
    let begun = begin_antigravity_authorization(&client_id);
    let payload = serde_json::to_value(&begun.pending).expect("pending serializes");
    if let Some(failed) = store_pending(state, session, provider, login_id, &payload, expires).await {
        return failed;
    }
    Json(json!({
        "login_id": login_id,
        "authorization_url": begun.authorization_url,
        "instructions": "Open authorization_url, approve, then paste the localhost callback URL (or just its code) from the browser.",
    }))
    .into_response()
}

async fn grok_login(
    state: &AppState,
    session: &SessionUser,
    provider: ProviderId,
    login_id: &str,
    expires: &str,
) -> Response {
    let client_id = state
        .config()
        .grok_oauth_client_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(GROK_DEFAULT_CLIENT_ID)
        .to_string();
    let form = form_encode(&[("client_id", client_id.as_str()), ("scope", GROK_SCOPE)]);
    let request = UpstreamRequest::post(GROK_DEVICE_CODE_URL)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form.into());
    let response = match state.transport().send(request).await {
        Ok(response) => response,
        Err(e) => return error(StatusCode::BAD_GATEWAY, &format!("device code failed: {e}")),
    };
    if !response.status.is_success() {
        let status = response.status.as_u16();
        let text = response.text().await.unwrap_or_default();
        return error(StatusCode::BAD_GATEWAY, &format!("device code failed: {status} {text}"));
    }
    let device = response.json_value().await.unwrap_or(Value::Null);
    let device_code = device.get("device_code").and_then(Value::as_str).unwrap_or_default();
    let payload =
        json!({ "client_id": client_id, "device_code": device_code, "token_endpoint": GROK_TOKEN_URL });
    if let Some(failed) = store_pending(state, session, provider, login_id, &payload, expires).await {
        return failed;
    }
    Json(json!({
        "login_id": login_id,
        "user_code": device.get("user_code").cloned().unwrap_or(Value::Null),
        "verification_uri": device.get("verification_uri").cloned().unwrap_or(Value::Null),
        "verification_uri_complete": device.get("verification_uri_complete").cloned().unwrap_or(Value::Null),
        "interval": device.get("interval").cloned().unwrap_or(json!(5)),
    }))
    .into_response()
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        ser.append_pair(k, v);
    }
    ser.finish()
}

// ------------------------------------------------ POST /:provider/login/:id/complete

/// The pending login for this (user, provider): by id first, then — because the dialog may
/// carry the wrong id — by the `oauth_state` the pasted callback returned. Expired rows never
/// match.
async fn load_pending_login(
    state: &AppState,
    user_id: &str,
    provider: ProviderId,
    login_id: &str,
    oauth_state: Option<&str>,
) -> Option<OAuthLoginStateRow> {
    let now = state.now_ms();
    let live = |row: &OAuthLoginStateRow| parse_iso_ms(&row.expires_at).is_some_and(|at| at >= now);
    if let Ok(Some(row)) = get_provider_state(state.pool(), user_id, provider.as_str(), login_id).await {
        if live(&row) {
            return Some(row);
        }
    }
    let oauth_state = oauth_state?;
    let rows = list_provider_states(state.pool(), user_id, provider.as_str()).await.ok()?;
    rows.into_iter().find(|row| {
        live(row)
            && serde_json::from_str::<Value>(&row.payload_json)
                .ok()
                .and_then(|p| p.get("oauth_state").and_then(Value::as_str).map(str::to_string))
                .as_deref()
                == Some(oauth_state)
    })
}

fn expiry_from_tokens(tokens: &OAuthTokens, now_ms: i64) -> Option<String> {
    tokens.expires_in.map(|secs| iso_from_ms(now_ms + secs * 1000))
}

/// Everything one completed login binds, beyond the ids the caller already holds.
struct BoundAccount<'a> {
    credential: StoredCredential,
    label: String,
    external_account_id: Option<&'a str>,
    meta: Value,
}

/// Persists the newly bound account and consumes the pending login row.
async fn finish_login(
    state: &AppState,
    user_id: &str,
    provider: ProviderId,
    state_row_id: &str,
    bound: BoundAccount<'_>,
) -> Response {
    let BoundAccount { credential, label, external_account_id, meta } = bound;
    let Ok(encrypted) = encrypt_json(state.config().token_encryption_key.as_deref(), &credential) else {
        return error(StatusCode::BAD_REQUEST, "TOKEN_ENCRYPTION_KEY not configured");
    };
    let meta_json = meta.to_string();
    let row = match insert_account(
        state.pool(),
        NewAccount {
            user_id,
            provider: provider.as_str(),
            encrypted_payload: &encrypted,
            label: Some(&label),
            external_account_id,
            account_meta_json: Some(&meta_json),
            priority: None,
        },
    )
    .await
    {
        Ok(row) => row,
        Err(_) => return internal_error(),
    };
    let _ = delete_state(state.pool(), state_row_id).await;
    Json(json!({ "ok": true, "token_id": row.id, "label": label })).into_response()
}

async fn login_complete(
    State(state): State<AppState>,
    session: SessionUser,
    Path((provider, login_id)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let Some(provider) = ProviderId::parse(&provider) else { return invalid_provider() };
    let body = object_body(&body);
    let raw = body
        .get("code")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| body.get("value").and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string();

    match provider {
        ProviderId::ClaudeCode => claude_complete(&state, &session, &login_id, &raw).await,
        ProviderId::Codex => codex_complete(&state, &session, &login_id).await,
        ProviderId::Antigravity => antigravity_complete(&state, &session, &login_id, &raw).await,
        ProviderId::Grok => grok_complete(&state, &session, &login_id).await,
    }
}

fn expired_login() -> Response {
    error(StatusCode::BAD_REQUEST, "login expired; start again")
}

async fn claude_complete(state: &AppState, session: &SessionUser, login_id: &str, raw: &str) -> Response {
    let provider = ProviderId::ClaudeCode;
    let (code, oauth_state) = match parse_code_hash_state(raw) {
        Ok(parsed) => parsed,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    let Some(state_row) =
        load_pending_login(state, &session.user.id, provider, login_id, Some(&oauth_state)).await
    else {
        return expired_login();
    };
    let Ok(pending) = serde_json::from_str::<PendingOAuth>(&state_row.payload_json) else {
        return error(StatusCode::BAD_REQUEST, "complete failed");
    };
    let tokens = match exchange_claude_code(state, &code, &oauth_state, &pending).await {
        Ok(tokens) => tokens,
        Err(e) => return error(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let identity = fetch_claude_identity(state, &tokens.access_token).await;
    let label = pick_account_label(identity.email.as_deref(), identity.display_name.as_deref(), "claude");
    let credential = StoredCredential {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        expires_at: expiry_from_tokens(&tokens, state.now_ms()),
        client_id: Some(pending.client_id.clone()),
        email: identity.email.clone(),
        ..Default::default()
    };
    let meta = json!({ "email": identity.email, "display_name": identity.display_name });
    let bound = BoundAccount { credential, label, external_account_id: None, meta };
    finish_login(state, &session.user.id, provider, &state_row.id, bound).await
}

async fn codex_complete(state: &AppState, session: &SessionUser, login_id: &str) -> Response {
    let provider = ProviderId::Codex;
    let Some(state_row) = load_pending_login(state, &session.user.id, provider, login_id, None).await else {
        return expired_login();
    };
    let pending: Value = serde_json::from_str(&state_row.payload_json).unwrap_or(Value::Null);
    let field = |key: &str| pending.get(key).and_then(Value::as_str).map(str::to_string);
    let (Some(client_id), Some(device_auth_id), Some(user_code)) =
        (field("client_id"), field("device_auth_id"), field("user_code"))
    else {
        return error(StatusCode::BAD_REQUEST, "invalid Codex device login state; start again");
    };

    let token_request = UpstreamRequest::post(CodexDeviceAuth::DEVICE_TOKEN_URL)
        .header("accept", "application/json")
        .json(&json!({ "device_auth_id": device_auth_id, "user_code": user_code }));
    let token_response = match state.transport().send(token_request).await {
        Ok(response) => response,
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("Codex device token failed: {e}")),
    };
    let token_status = token_response.status;
    let token_text = token_response.text().await.unwrap_or_default();
    // The pending 403/404 responses may not be JSON.
    let device_token: Value = serde_json::from_str(&token_text).unwrap_or(Value::Null);
    let device_error = device_token.get("error").and_then(Value::as_str);
    if device_error == Some("authorization_pending") || device_error == Some("slow_down") {
        return error(StatusCode::BAD_REQUEST, device_error.unwrap_or_default());
    }
    if token_status == StatusCode::FORBIDDEN || token_status == StatusCode::NOT_FOUND {
        return error(StatusCode::BAD_REQUEST, &format!("token {}", token_status.as_u16()));
    }
    if !token_status.is_success() {
        let detail = device_error.map(str::to_string).unwrap_or_else(|| token_text.trim().to_string());
        let detail = if detail.is_empty() { format!("HTTP {}", token_status.as_u16()) } else { detail };
        return error(StatusCode::BAD_REQUEST, &format!("Codex device token failed: {detail}"));
    }
    let Some(authorization_code) =
        device_token.get("authorization_code").and_then(Value::as_str).filter(|s| !s.is_empty())
    else {
        return error(StatusCode::BAD_REQUEST, "Codex device token response missing authorization_code");
    };
    let Some(code_verifier) = device_token.get("code_verifier").and_then(Value::as_str).filter(|s| !s.is_empty())
    else {
        return error(StatusCode::BAD_REQUEST, "Codex device token response missing code_verifier");
    };

    let form = form_encode(&[
        ("grant_type", "authorization_code"),
        ("client_id", &client_id),
        ("code", authorization_code),
        ("code_verifier", code_verifier),
        ("redirect_uri", CodexDeviceAuth::REDIRECT_URI),
    ]);
    let exchange = UpstreamRequest::post(CodexDeviceAuth::TOKEN_URL)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(form.into());
    let exchange_response = match state.transport().send(exchange).await {
        Ok(response) => response,
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("Codex OAuth token exchange failed: {e}")),
    };
    if !exchange_response.status.is_success() {
        let status = exchange_response.status.as_u16();
        let detail = exchange_response.text().await.unwrap_or_default().trim().to_string();
        let detail = if detail.is_empty() { format!("HTTP {status}") } else { detail };
        return error(StatusCode::BAD_REQUEST, &format!("Codex OAuth token exchange failed: {detail}"));
    }
    let tokens: OAuthTokens = match exchange_response.json_value().await.map(serde_json::from_value) {
        Ok(Ok(tokens)) => tokens,
        _ => return error(StatusCode::BAD_REQUEST, "complete failed"),
    };

    let account_id = extract_chatgpt_account_id(&tokens.access_token);
    // `codex_identity` is pure; the usage payload it reads for the plan comes from the same
    // endpoint the adapter uses, so this module makes no provider-specific call of its own.
    let usage_payload = match account_id.as_deref() {
        Some(id) if !id.is_empty() => fetch_codex_usage_json(state, &tokens.access_token, id)
            .await
            .payload
            .as_ref()
            .map(usage_payload_value),
        _ => None,
    };
    let identity = codex_identity(&tokens.access_token, account_id.as_deref(), usage_payload.as_ref());
    let fallback = match account_id.as_deref() {
        Some(id) => format!("codex:{}", id.chars().take(8).collect::<String>()),
        None => "codex".to_string(),
    };
    let label = pick_account_label(identity.email.as_deref(), identity.display_name.as_deref(), &fallback);
    let credential = StoredCredential {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        expires_at: extract_jwt_expiry_iso(&tokens.access_token).or_else(|| expiry_from_tokens(&tokens, state.now_ms())),
        account_id: account_id.clone(),
        client_id: Some(client_id),
        email: identity.email.clone(),
        ..Default::default()
    };
    let meta = json!({ "email": identity.email, "plan_type": identity.plan, "account_id": account_id });
    let bound = BoundAccount { credential, label, external_account_id: account_id.as_deref(), meta };
    finish_login(state, &session.user.id, provider, &state_row.id, bound).await
}

async fn antigravity_complete(state: &AppState, session: &SessionUser, login_id: &str, raw: &str) -> Response {
    let provider = ProviderId::Antigravity;
    let (code, oauth_state) = match parse_antigravity_callback(raw) {
        Ok(parsed) => parsed,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    let Some(state_row) = load_pending_login(state, &session.user.id, provider, login_id, oauth_state.as_deref()).await
    else {
        return expired_login();
    };
    let Some((_, client_secret)) = antigravity_oauth_client(state) else {
        return error(StatusCode::BAD_REQUEST, "Antigravity is not configured on this deploy.");
    };
    let Ok(pending) = serde_json::from_str::<PendingAntigravityOAuth>(&state_row.payload_json) else {
        return error(StatusCode::BAD_REQUEST, "complete failed");
    };
    let tokens = match exchange_antigravity_code(state, &code, oauth_state.as_deref(), &pending, &client_secret).await {
        Ok(tokens) => tokens,
        Err(e) => return error(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let identity = fetch_antigravity_identity(state, &tokens.access_token).await;
    let label = pick_account_label(identity.email.as_deref(), identity.display_name.as_deref(), "antigravity");
    // Resolve the CloudCode project once, here, so the dispatch path never pays for
    // loadCodeAssist/onboardUser (docs/providers.md § Antigravity). A failure must not lose the
    // tokens the user just approved — the adapter retries the bootstrap on first use.
    let project = bootstrap_antigravity_project(state, &tokens.access_token).await.ok();
    let mut credential = StoredCredential {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        expires_at: expiry_from_tokens(&tokens, state.now_ms()),
        client_id: Some(pending.client_id.clone()),
        token_endpoint: Some(AntigravityOAuth::TOKEN_URL.to_string()),
        email: identity.email.clone(),
        ..Default::default()
    };
    if let Some(project) = &project {
        let mut extra = Map::new();
        extra.insert("project_id".into(), json!(project.project_id));
        extra.insert("tier_id".into(), json!(project.tier_id));
        credential.extra = Some(extra);
    }
    let meta = json!({
        "email": identity.email,
        "display_name": identity.display_name,
        "plan_type": project.as_ref().and_then(|p| p.tier_id.clone()),
    });
    let bound = BoundAccount { credential, label, external_account_id: None, meta };
    finish_login(state, &session.user.id, provider, &state_row.id, bound).await
}

async fn grok_complete(state: &AppState, session: &SessionUser, login_id: &str) -> Response {
    let provider = ProviderId::Grok;
    let Some(state_row) = load_pending_login(state, &session.user.id, provider, login_id, None).await else {
        return expired_login();
    };
    let payload: Value = serde_json::from_str(&state_row.payload_json).unwrap_or(Value::Null);
    let field = |key: &str| payload.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    let token_endpoint = {
        let stored = field("token_endpoint");
        if stored.is_empty() {
            GROK_TOKEN_URL.to_string()
        } else {
            stored
        }
    };
    let client_id = field("client_id");
    let form = form_encode(&[
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("device_code", &field("device_code")),
        ("client_id", &client_id),
    ]);
    let request = UpstreamRequest::post(token_endpoint.clone())
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form.into());
    let response = match state.transport().send(request).await {
        Ok(response) => response,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": "token 0", "detail": e.to_string() }))).into_response()
        }
    };
    if !response.status.is_success() {
        let status = response.status.as_u16();
        let detail = response.text().await.unwrap_or_default();
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("token {status}"), "detail": detail })),
        )
            .into_response();
    }
    let tokens: OAuthTokens = match response.json_value().await.map(serde_json::from_value) {
        Ok(Ok(tokens)) => tokens,
        _ => return error(StatusCode::BAD_REQUEST, "complete failed"),
    };
    let identity = fetch_grok_identity(state, &tokens.access_token).await;
    let label = pick_account_label(identity.email.as_deref(), identity.display_name.as_deref(), "grok");
    let credential = StoredCredential {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        expires_at: expiry_from_tokens(&tokens, state.now_ms()),
        client_id: (!client_id.is_empty()).then_some(client_id),
        token_endpoint: Some(token_endpoint),
        email: identity.email.clone(),
        ..Default::default()
    };
    let meta = json!({ "email": identity.email, "display_name": identity.display_name });
    let bound = BoundAccount { credential, label, external_account_id: None, meta };
    finish_login(state, &session.user.id, provider, &state_row.id, bound).await
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::session::create_session;
    use crate::db::accounts::iso_from_ms;
    use crate::db::test_support::{
        insert_user, skip_without_db, test_config, test_pool, test_router, test_state, TEST_TOKEN_KEY,
    };
    use crate::db::users::UserRow;
    use crate::pool::extension::{ListSharedOptions, PoolExtension, ReserveContext, ReserveOutcome, ShareInfo};
    use crate::providers::AccountUsage;
    use crate::routing::types::RoutingCandidate;
    use crate::upstream::{MockTransport, TransportError, UpstreamResponse};
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{header, Request};
    use base64::Engine;
    use http_body_util::BodyExt;
    use sqlx::PgPool;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    const NOW: &str = "2026-01-01T00:00:00.000Z";

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn signed_in(state: &AppState, email: &str) -> (UserRow, String) {
        let user = insert_user(state.pool(), email).await;
        let (_, cookie) = create_session(state, &user.id, false).await.unwrap();
        (user, cookie.split(';').next().unwrap().to_string())
    }

    fn request(method: &str, uri: &str, cookie: Option<&str>, body: Option<Value>) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "app.example.com")
            .header(header::ORIGIN, "https://app.example.com")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        builder.body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty)).unwrap()
    }

    async fn call(state: &AppState, req: Request<Body>) -> Response {
        test_router(state.clone()).oneshot(req).await.unwrap()
    }

    /// `seedAccount` from the vitest suite: one `upstream_accounts` row with a known payload,
    /// a stored upstream label, and whatever profile facts the case needs.
    async fn seed_account(pool: &PgPool, user_id: &str, id: &str, provider: &str, priority: i32, meta: Value) {
        seed_account_at(pool, user_id, id, provider, priority, meta, NOW).await
    }

    async fn seed_account_at(
        pool: &PgPool,
        user_id: &str,
        id: &str,
        provider: &str,
        priority: i32,
        meta: Value,
        created_at: &str,
    ) {
        let blob =
            encrypt_json(Some(TEST_TOKEN_KEY), &StoredCredential { access_token: "tok_test".into(), ..Default::default() })
                .unwrap();
        sqlx::query(
            "INSERT INTO upstream_accounts
             (id, user_id, provider, external_account_id, label, custom_label, priority, encrypted_payload,
              account_meta_json, created_at, updated_at)
             VALUES ($1, $2, $3, NULL, 'old-upstream-label', NULL, $4, $5, $6, $7, $7)",
        )
        .bind(id)
        .bind(user_id)
        .bind(provider)
        .bind(priority)
        .bind(&blob)
        .bind(meta.to_string())
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
    }

    fn account_meta(email: &str) -> Value {
        json!({ "email": email })
    }

    /// Answers the two concurrent GETs `claude-code`'s `fetch_usage` makes, by URL rather than
    /// by call order, `rounds` times. `calls` counts only the usage reads — the whole point of
    /// the server-side cache is how many of those reach upstream.
    fn stub_claude(
        transport: &Arc<MockTransport>,
        rounds: usize,
        email: &str,
        calls: Arc<AtomicUsize>,
        usage: Arc<dyn Fn(usize) -> (StatusCode, Value) + Send + Sync>,
    ) {
        for _ in 0..(rounds * 2) {
            let email = email.to_string();
            let calls = calls.clone();
            let usage = usage.clone();
            transport.expect(move |req| {
                if req.url.contains("/api/oauth/usage") {
                    let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    let (status, body) = usage(n);
                    Ok(UpstreamResponse::json(status, &body))
                } else if req.url.contains("/api/oauth/profile") {
                    Ok(UpstreamResponse::json(StatusCode::OK, &json!({ "account": { "email": email } })))
                } else {
                    panic!("unexpected upstream request {}", req.url)
                }
            });
        }
    }

    /// The fixed 10%-utilization stub, the suite's `stubClaudeUsage`.
    fn stub_claude_usage(transport: &Arc<MockTransport>, rounds: usize, email: &str) -> Arc<AtomicUsize> {
        let calls = Arc::new(AtomicUsize::new(0));
        stub_claude(
            transport,
            rounds,
            email,
            calls.clone(),
            Arc::new(|_| (StatusCode::OK, json!({ "five_hour": { "utilization": 10 } }))),
        );
        calls
    }

    /// `countingClaudeUsage`: utilization varies per call so a cached response is
    /// distinguishable from a fresh one.
    fn counting_claude_usage(transport: &Arc<MockTransport>, rounds: usize) -> Arc<AtomicUsize> {
        let calls = Arc::new(AtomicUsize::new(0));
        stub_claude(
            transport,
            rounds,
            "u@example.com",
            calls.clone(),
            Arc::new(|n| (StatusCode::OK, json!({ "five_hour": { "utilization": n * 10 } }))),
        );
        calls
    }

    async fn stored_row(pool: &PgPool, user_id: &str, id: &str) -> AccountRow {
        get_account(pool, user_id, id).await.unwrap().unwrap()
    }

    // ------------------------------------------- PATCH /:provider/accounts/:id custom_label

    #[tokio::test]
    async fn sets_a_custom_label_and_the_list_returns_it_as_label_and_custom_label() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        let before = stored_row(&pool, &user.id, "acc_1").await;

        let patch = call(
            &state,
            request("PATCH", "/api/providers/claude-code/accounts/acc_1", Some(&cookie), Some(json!({ "custom_label": "  Work Claude  " }))),
        )
        .await;
        assert_eq!(patch.status(), StatusCode::OK);
        assert_eq!(body_json(patch).await, json!({ "ok": true, "custom_label": "Work Claude" }));

        let after = stored_row(&pool, &user.id, "acc_1").await;
        assert_eq!(after.custom_label.as_deref(), Some("Work Claude"));
        assert_eq!(after.label, before.label);
        assert_eq!(after.priority, before.priority);
        assert_eq!(after.encrypted_payload, before.encrypted_payload);
        assert_eq!(after.account_meta_json, before.account_meta_json);

        stub_claude_usage(&transport, 1, "upstream@example.com");
        let get = call(&state, request("GET", "/api/providers/claude-code/accounts", Some(&cookie), None)).await;
        assert_eq!(get.status(), StatusCode::OK);
        let json = body_json(get).await;
        assert_eq!(json["accounts"][0]["label"], "Work Claude");
        assert_eq!(json["accounts"][0]["custom_label"], "Work Claude");
        assert_eq!(json["accounts"][0]["account"]["email"], "upstream@example.com");
    }

    #[tokio::test]
    async fn a_custom_label_survives_an_identity_sync_on_the_next_read() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;

        let patch = call(
            &state,
            request("PATCH", "/api/providers/claude-code/accounts/acc_1", Some(&cookie), Some(json!({ "custom_label": "Keep this name" }))),
        )
        .await;
        assert_eq!(patch.status(), StatusCode::OK);

        stub_claude_usage(&transport, 1, "first@example.com");
        call(&state, request("GET", "/api/providers/claude-code/accounts", Some(&cookie), None)).await;
        stub_claude_usage(&transport, 1, "changed@example.com");
        // ?refresh=true: the identity sync under test only runs on a live fetch, and a plain
        // second read inside the TTL is served from the usage cache.
        let get = call(&state, request("GET", "/api/providers/claude-code/accounts?refresh=true", Some(&cookie), None)).await;

        let json = body_json(get).await;
        assert_eq!(json["accounts"][0]["label"], "Keep this name");
        assert_eq!(json["accounts"][0]["custom_label"], "Keep this name");
        assert_eq!(json["accounts"][0]["account"]["email"], "changed@example.com");
        let row = stored_row(&pool, &user.id, "acc_1").await;
        assert_eq!(row.label.as_deref(), Some("changed@example.com"));
        assert_eq!(row.custom_label.as_deref(), Some("Keep this name"));
    }

    #[tokio::test]
    async fn null_and_the_empty_string_both_clear_the_custom_label() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        stub_claude_usage(&transport, 2, "fallback@example.com");

        let uri = "/api/providers/claude-code/accounts/acc_1";
        assert_eq!(
            call(&state, request("PATCH", uri, Some(&cookie), Some(json!({ "custom_label": "Temporary name" })))).await.status(),
            StatusCode::OK
        );
        let cleared = call(&state, request("PATCH", uri, Some(&cookie), Some(json!({ "custom_label": null })))).await;
        assert_eq!(cleared.status(), StatusCode::OK);
        assert_eq!(body_json(cleared).await, json!({ "ok": true, "custom_label": null }));
        let listed = call(&state, request("GET", "/api/providers/claude-code/accounts", Some(&cookie), None)).await;
        let json = body_json(listed).await;
        assert_eq!(json["accounts"][0]["label"], "fallback@example.com");
        assert_eq!(json["accounts"][0]["custom_label"], Value::Null);

        assert_eq!(
            call(&state, request("PATCH", uri, Some(&cookie), Some(json!({ "custom_label": "Temporary again" })))).await.status(),
            StatusCode::OK
        );
        let cleared = call(&state, request("PATCH", uri, Some(&cookie), Some(json!({ "custom_label": "" })))).await;
        assert_eq!(cleared.status(), StatusCode::OK);
        assert_eq!(body_json(cleared).await, json!({ "ok": true, "custom_label": null }));
        let listed = call(&state, request("GET", "/api/providers/claude-code/accounts?refresh=true", Some(&cookie), None)).await;
        let json = body_json(listed).await;
        assert_eq!(json["accounts"][0]["label"], "fallback@example.com");
        assert_eq!(json["accounts"][0]["custom_label"], Value::Null);
    }

    #[tokio::test]
    async fn a_bad_custom_label_is_refused_and_never_crosses_users() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        let uri = "/api/providers/claude-code/accounts/acc_1";

        let long = call(&state, request("PATCH", uri, Some(&cookie), Some(json!({ "custom_label": "x".repeat(65) })))).await;
        assert_eq!(long.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(long).await, json!({ "error": "custom_label too long" }));

        let typed = call(&state, request("PATCH", uri, Some(&cookie), Some(json!({ "custom_label": 42 })))).await;
        assert_eq!(typed.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(typed).await, json!({ "error": "invalid custom_label" }));

        // Another user's account id is a 404, never an edit.
        let other = insert_user(&pool, "user_2@example.com").await;
        seed_account(&pool, &other.id, "acc_user_2", "claude-code", 7, account_meta("other@example.com")).await;
        let theirs = call(
            &state,
            request("PATCH", "/api/providers/claude-code/accounts/acc_user_2", Some(&cookie), Some(json!({ "custom_label": "not yours" }))),
        )
        .await;
        assert_eq!(theirs.status(), StatusCode::NOT_FOUND);
        assert_eq!(stored_row(&pool, &other.id, "acc_user_2").await.custom_label, None);

        // And the whole group requires a session.
        let anon = call(&state, request("PATCH", uri, None, Some(json!({ "custom_label": "no session" })))).await;
        assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
    }

    // ------------------------------------------------------------------- Codex device OAuth

    fn codex_jwt(claims: Value) -> String {
        let mid = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("eyJhbGciOiJub25lIn0.{mid}.signature")
    }

    fn codex_state(pool_state: &AppState, client_id: Option<&str>) -> AppState {
        let mut config = test_config();
        config.codex_oauth_client_id = client_id.map(str::to_string);
        AppState::builder(config, pool_state.pool().clone())
            .transport(pool_state.transport().clone())
            .build()
    }

    async fn seed_codex_login(pool: &PgPool, user_id: &str, login_id: &str, client_id: &str) {
        insert_provider_state(
            pool,
            login_id,
            user_id,
            "codex",
            &json!({ "client_id": client_id, "device_auth_id": "device-auth-id", "user_code": "ABCD-EFGH" }).to_string(),
            &iso_from_ms(crate::app::now_ms() + 60_000),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn codex_login_starts_the_device_flow_with_json_and_returns_the_polling_shape() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let base = test_state(pool.clone(), transport.clone());
        let state = codex_state(&base, Some("codex-client-override"));
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        transport.respond_json(
            StatusCode::OK,
            json!({ "device_auth_id": "device-auth-id", "user_code": "ABCD-EFGH", "interval": "7" }),
        );

        let res = call(&state, request("POST", "/api/providers/codex/login", Some(&cookie), None)).await;
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert!(json["login_id"].as_str().is_some());
        assert_eq!(json["user_code"], "ABCD-EFGH");
        assert_eq!(json["verification_uri"], CODEX_VERIFICATION_URI);
        assert_eq!(json["interval"], 7);

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, CodexDeviceAuth::USER_CODE_URL);
        assert_eq!(requests[0].method, axum::http::Method::POST);
        // A form body is rejected by OpenAI with model_attributes_type.
        assert_eq!(requests[0].header("content-type"), Some("application/json"));
        assert_eq!(requests[0].header("accept"), Some("application/json"));
        assert_eq!(requests[0].json(), json!({ "client_id": "codex-client-override" }));

        let stored: String = sqlx::query_scalar("SELECT payload_json FROM oauth_login_states WHERE user_id = $1")
            .bind(&user.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&stored).unwrap(),
            json!({ "client_id": "codex-client-override", "device_auth_id": "device-auth-id", "user_code": "ABCD-EFGH" })
        );
    }

    #[tokio::test]
    async fn codex_login_defaults_the_polling_interval_when_openai_omits_it() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;
        transport.respond_json(StatusCode::OK, json!({ "device_auth_id": "device-auth-id", "user_code": "ABCD-EFGH" }));

        let res = call(&state, request("POST", "/api/providers/codex/login", Some(&cookie), None)).await;
        assert_eq!(body_json(res).await["interval"], 5);
    }

    #[tokio::test]
    async fn codex_complete_returns_a_nested_403_as_pending_without_exchanging_tokens() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_codex_login(&pool, &user.id, "login_pending", "codex-client").await;
        transport.respond_json(
            StatusCode::FORBIDDEN,
            json!({ "error": { "message": "Authorization pending", "code": "deviceauth_authorization_pending" } }),
        );

        let res = call(&state, request("POST", "/api/providers/codex/login/login_pending/complete", Some(&cookie), None)).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(res).await, json!({ "error": "token 403" }));

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, CodexDeviceAuth::DEVICE_TOKEN_URL);
        assert_eq!(requests[0].header("content-type"), Some("application/json"));
        assert_eq!(requests[0].header("accept"), Some("application/json"));
        assert_eq!(requests[0].json(), json!({ "device_auth_id": "device-auth-id", "user_code": "ABCD-EFGH" }));
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM oauth_login_states").fetch_one(&pool).await.unwrap();
        assert_eq!(rows, 1, "the pending login survives a poll");
    }

    #[tokio::test]
    async fn codex_complete_treats_slow_down_and_404_as_pending_and_names_a_real_failure() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;

        seed_codex_login(&pool, &user.id, "login_slow_down", "client").await;
        transport.respond_json(StatusCode::BAD_REQUEST, json!({ "error": "slow_down" }));
        let res = call(&state, request("POST", "/api/providers/codex/login/login_slow_down/complete", Some(&cookie), None)).await;
        assert_eq!(body_json(res).await, json!({ "error": "slow_down" }));

        seed_codex_login(&pool, &user.id, "login_404", "client").await;
        transport.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::NOT_FOUND, Default::default(), "not ready")));
        let res = call(&state, request("POST", "/api/providers/codex/login/login_404/complete", Some(&cookie), None)).await;
        assert_eq!(body_json(res).await, json!({ "error": "token 404" }));

        seed_codex_login(&pool, &user.id, "login_failed", "client").await;
        transport.respond_json(StatusCode::BAD_REQUEST, json!({ "error": "access_denied" }));
        let res = call(&state, request("POST", "/api/providers/codex/login/login_failed/complete", Some(&cookie), None)).await;
        assert_eq!(body_json(res).await, json!({ "error": "Codex device token failed: access_denied" }));
    }

    #[tokio::test]
    async fn codex_complete_exchanges_the_approved_code_with_the_server_issued_verifier() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_codex_login(&pool, &user.id, "login_approved", "codex-client").await;
        let jwt = codex_jwt(json!({
            "exp": 1_800_000_000,
            "email": "codex@example.com",
            "https://api.openai.com/auth": { "chatgpt_account_id": "account-123" },
        }));

        transport.respond_json(
            StatusCode::OK,
            json!({ "authorization_code": "approved-code", "code_verifier": "upstream-verifier" }),
        );
        let token_body = json!({ "access_token": jwt, "refresh_token": "refresh-token", "expires_in": 3600 });
        transport.expect(move |_| Ok(UpstreamResponse::json(StatusCode::OK, &token_body)));
        // The usage probe behind `codex_identity` is bot-walled; the JWT still names the email.
        for _ in 0..2 {
            transport.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::FORBIDDEN, Default::default(), "not available")));
        }

        let res = call(&state, request("POST", "/api/providers/codex/login/login_approved/complete", Some(&cookie), None)).await;
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["ok"], true);
        assert_eq!(json["label"], "codex@example.com");
        assert!(json["token_id"].as_str().is_some());

        let requests = transport.requests();
        assert_eq!(requests[0].url, CodexDeviceAuth::DEVICE_TOKEN_URL);
        assert_eq!(requests[1].url, CodexDeviceAuth::TOKEN_URL);
        assert_eq!(requests[1].header("content-type"), Some("application/x-www-form-urlencoded"));
        assert_eq!(requests[1].header("accept"), Some("application/json"));
        let form: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(requests[1].body.as_deref().unwrap()).into_owned().collect();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["client_id"], "codex-client");
        assert_eq!(form["code"], "approved-code");
        assert_eq!(form["code_verifier"], "upstream-verifier");
        assert_eq!(form["redirect_uri"], CodexDeviceAuth::REDIRECT_URI);

        let rows = list_accounts(&pool, &user.id, "codex").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].external_account_id.as_deref(), Some("account-123"));
        assert_eq!(rows[0].label.as_deref(), Some("codex@example.com"));
        assert_eq!(
            rows[0].account_meta_json.as_deref(),
            Some(r#"{"email":"codex@example.com","plan_type":null,"account_id":"account-123"}"#)
        );
        let credential: StoredCredential =
            crate::crypto::token_crypto::decrypt_json(Some(TEST_TOKEN_KEY), &rows[0].encrypted_payload).unwrap();
        assert_eq!(credential.access_token, jwt);
        assert_eq!(credential.refresh_token.as_deref(), Some("refresh-token"));
        assert_eq!(credential.account_id.as_deref(), Some("account-123"));
        assert_eq!(credential.client_id.as_deref(), Some("codex-client"));
        assert_eq!(credential.email.as_deref(), Some("codex@example.com"));
        assert_eq!(credential.expires_at.as_deref(), Some("2027-01-15T08:00:00.000Z"));
        let states: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM oauth_login_states").fetch_one(&pool).await.unwrap();
        assert_eq!(states, 0, "the pending login is consumed");
    }

    // --------------------------------------------------- GET /:provider/accounts usage cache

    async fn claude_accounts(state: &AppState, cookie: &str, refresh: bool) -> Value {
        let uri = if refresh { "/api/providers/claude-code/accounts?refresh=true" } else { "/api/providers/claude-code/accounts" };
        body_json(call(state, request("GET", uri, Some(cookie), None)).await).await
    }

    #[tokio::test]
    async fn a_second_read_inside_the_ttl_is_served_from_cache() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        let calls = counting_claude_usage(&transport, 2);

        let first = claude_accounts(&state, &cookie, false).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first["accounts"][0]["usage"]["windows"][0]["utilization"], 10.0);

        // A second device polling inside the TTL must not reach upstream, and must see the
        // same numbers the first one saw.
        let second = claude_accounts(&state, &cookie, false).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(second["accounts"][0]["usage"]["windows"][0]["utilization"], 10.0);
    }

    #[tokio::test]
    async fn a_stale_snapshot_refetches_synchronously_and_returns_the_fresh_value() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        let calls = counting_claude_usage(&transport, 2);

        claude_accounts(&state, &cookie, false).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        age_usage(&pool, "acc_1", 121_000).await;

        let after = claude_accounts(&state, &cookie, false).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(after["accounts"][0]["usage"]["windows"][0]["utilization"], 20.0);
        assert_eq!(after["accounts"][0]["stale"], false);
    }

    async fn age_usage(pool: &PgPool, id: &str, by_ms: i64) {
        sqlx::query("UPDATE upstream_accounts SET usage_fetched_at = $1 WHERE id = $2")
            .bind(iso_from_ms(crate::app::now_ms() - by_ms))
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn refresh_true_bypasses_the_cache() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        let calls = counting_claude_usage(&transport, 2);

        claude_accounts(&state, &cookie, false).await;
        let refreshed = claude_accounts(&state, &cookie, true).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(refreshed["accounts"][0]["usage"]["windows"][0]["utilization"], 20.0);
    }

    #[tokio::test]
    async fn a_reader_that_loses_the_lock_serves_the_stored_snapshot() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        let calls = counting_claude_usage(&transport, 2);

        claude_accounts(&state, &cookie, false).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Stale snapshot + a lock someone else is holding: the loser must return the old value
        // rather than queue up a second upstream call.
        age_usage(&pool, "acc_1", 121_000).await;
        sqlx::query("UPDATE upstream_accounts SET usage_fetching_at = $1 WHERE id = 'acc_1'")
            .bind(now_iso())
            .execute(&pool)
            .await
            .unwrap();

        let res = claude_accounts(&state, &cookie, false).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(res["accounts"][0]["usage"]["windows"][0]["utilization"], 10.0);
        assert_eq!(res["accounts"][0]["stale"], true);
    }

    #[tokio::test]
    async fn a_lock_left_behind_by_a_dead_request_is_broken() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        let calls = counting_claude_usage(&transport, 1);
        // A lock older than 30s means its holder is gone; it must not wedge the account's
        // usage forever.
        sqlx::query("UPDATE upstream_accounts SET usage_fetching_at = $1 WHERE id = 'acc_1'")
            .bind(iso_from_ms(crate::app::now_ms() - 31_000))
            .execute(&pool)
            .await
            .unwrap();

        claude_accounts(&state, &cookie, false).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(stored_row(&pool, &user.id, "acc_1").await.usage_fetching_at.is_none());
    }

    #[tokio::test]
    async fn an_upstream_failure_keeps_the_previous_windows_and_releases_the_lock() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        counting_claude_usage(&transport, 1);

        claude_accounts(&state, &cookie, false).await;
        assert!(stored_row(&pool, &user.id, "acc_1").await.usage_snapshot_json.is_some());

        age_usage(&pool, "acc_1", 121_000).await;
        for _ in 0..2 {
            transport.expect(|_| Err(TransportError::Other("upstream down".into())));
        }

        let res = claude_accounts(&state, &cookie, false).await;
        assert!(res["accounts"][0]["error"].as_str().unwrap().contains("upstream down"));
        // One hiccup must not blank the bars for every device sharing this cache, and must not
        // leave the lock held.
        assert_eq!(res["accounts"][0]["usage"]["windows"][0]["utilization"], 10.0);
        assert_eq!(res["accounts"][0]["stale"], true);
        let row = stored_row(&pool, &user.id, "acc_1").await;
        assert_eq!(read_usage_snapshot(&row).unwrap().windows[0]["utilization"], 10.0);
        assert!(row.usage_fetching_at.is_none());
    }

    #[tokio::test]
    async fn a_cached_unusable_account_stays_unusable() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        for _ in 0..2 {
            transport.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::UNAUTHORIZED, Default::default(), "nope")));
        }

        let live = claude_accounts(&state, &cookie, false).await;
        assert_eq!(live["accounts"][0]["status"], "unusable");

        // The snapshot stores error/edgeBlocked precisely so the cached read re-derives the
        // same status instead of silently reporting "active".
        let cached = claude_accounts(&state, &cookie, false).await;
        assert_eq!(cached["accounts"][0]["status"], "unusable");
    }

    #[tokio::test]
    async fn an_exhausted_window_reads_limited_and_active_moves_to_the_next_account() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        // Higher priority sorts first — this is the pool's primary account.
        seed_account(&pool, &user.id, "acc_first", "claude-code", 9, account_meta("first@example.com")).await;
        seed_account(&pool, &user.id, "acc_second", "claude-code", 1, account_meta("second@example.com")).await;
        let resets_at = iso_from_ms(crate::app::now_ms() + 3 * 60 * 60 * 1000);
        let exhausted = json!({ "five_hour": { "utilization": 100, "resets_at": resets_at } });
        stub_claude(
            &transport,
            2,
            "u@example.com",
            Arc::new(AtomicUsize::new(0)),
            Arc::new(move |_| (StatusCode::OK, exhausted.clone())),
        );

        let both = claude_accounts(&state, &cookie, false).await;
        // Every account limited: nothing is Active, because nothing would serve a request
        // until a window resets (docs/providers.md § Routing module).
        let statuses: Vec<&str> =
            both["accounts"].as_array().unwrap().iter().map(|a| a["status"].as_str().unwrap()).collect();
        assert_eq!(statuses, vec!["limited", "limited"]);

        // Now only the primary is exhausted: Active moves to the account the ordered walk
        // would actually pick.
        sqlx::query("UPDATE upstream_accounts SET usage_snapshot_json = $1 WHERE id = 'acc_second'")
            .bind(
                json!({
                    "windows": [{ "label": "5h", "utilization": 41, "resets_at": resets_at }],
                    "account": {},
                    "error": null,
                    "stale": false,
                    "edgeBlocked": false,
                })
                .to_string(),
            )
            .execute(&pool)
            .await
            .unwrap();

        let mixed = claude_accounts(&state, &cookie, false).await;
        let rows: Vec<(&str, &str)> = mixed["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| (a["id"].as_str().unwrap(), a["status"].as_str().unwrap()))
            .collect();
        assert_eq!(rows, vec![("acc_first", "limited"), ("acc_second", "active")]);
    }

    #[tokio::test]
    async fn an_already_reset_window_is_not_a_limit_fact() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        // 100% but the reset is in the past: the skip self-expires even off a stale snapshot,
        // so this account is usable again.
        let past = iso_from_ms(crate::app::now_ms() - 1000);
        stub_claude(
            &transport,
            1,
            "u@example.com",
            Arc::new(AtomicUsize::new(0)),
            Arc::new(move |_| (StatusCode::OK, json!({ "five_hour": { "utilization": 100, "resets_at": past } }))),
        );

        let res = claude_accounts(&state, &cookie, false).await;
        assert_eq!(res["accounts"][0]["status"], "active");
    }

    // ----------------------------------------------------------------- PATCH /:provider

    #[tokio::test]
    async fn the_strategy_write_accepts_only_ordered_and_upserts_one_row() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;

        let anon = call(&state, request("PATCH", "/api/providers/claude-code", None, Some(json!({ "strategy": "ordered" })))).await;
        assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

        let bad_provider =
            call(&state, request("PATCH", "/api/providers/not-a-provider", Some(&cookie), Some(json!({ "strategy": "ordered" })))).await;
        assert_eq!(bad_provider.status(), StatusCode::BAD_REQUEST);

        let ok = call(&state, request("PATCH", "/api/providers/claude-code", Some(&cookie), Some(json!({ "strategy": "ordered" })))).await;
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(body_json(ok).await, json!({ "ok": true, "strategy": "ordered" }));
        let rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT user_id, provider, strategy FROM provider_settings").fetch_all(&pool).await.unwrap();
        assert_eq!(rows, vec![(user.id.clone(), "claude-code".to_string(), "ordered".to_string())]);

        // A second PATCH updates the existing row rather than inserting a duplicate.
        call(&state, request("PATCH", "/api/providers/claude-code", Some(&cookie), Some(json!({ "strategy": "ordered" })))).await;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM provider_settings").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1);

        // Any other value is refused and writes nothing new.
        let other = call(
            &state,
            request("PATCH", "/api/providers/grok", Some(&cookie), Some(json!({ "strategy": "usage-balanced" }))),
        )
        .await;
        assert_eq!(other.status(), StatusCode::BAD_REQUEST);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM provider_settings").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn the_accounts_list_carries_the_strategy_and_defaults_to_ordered() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;

        assert_eq!(claude_accounts(&state, &cookie, false).await["strategy"], "ordered");
        call(&state, request("PATCH", "/api/providers/claude-code", Some(&cookie), Some(json!({ "strategy": "ordered" })))).await;
        assert_eq!(claude_accounts(&state, &cookie, false).await["strategy"], "ordered");
    }

    // ------------------------------------------- POST /:provider/accounts/:id/unpause

    #[tokio::test]
    async fn unpause_is_scoped_to_the_owner_the_provider_and_a_real_row() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        let now = state.now_ms();

        let anon = call(&state, request("POST", "/api/providers/claude-code/accounts/acc_1/unpause", None, None)).await;
        assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

        let bad_provider =
            call(&state, request("POST", "/api/providers/my-endpoint/accounts/acc_1/unpause", Some(&cookie), None)).await;
        assert_eq!(bad_provider.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(bad_provider).await, json!({ "error": "invalid provider" }));

        let missing =
            call(&state, request("POST", "/api/providers/claude-code/accounts/acc_missing/unpause", Some(&cookie), None)).await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(missing).await, json!({ "error": "not found" }));

        // Another user's benched account stays benched.
        let other = insert_user(&pool, "user_2@example.com").await;
        seed_account(&pool, &other.id, "acc_user_2", "claude-code", 7, account_meta("o@example.com")).await;
        crate::pool::bench::mark_benched(&pool, &other.id, "claude-code", "acc_user_2", None, None, now).await.unwrap();
        let theirs =
            call(&state, request("POST", "/api/providers/claude-code/accounts/acc_user_2/unpause", Some(&cookie), None)).await;
        assert_eq!(theirs.status(), StatusCode::NOT_FOUND);
        assert!(crate::pool::bench::is_benched(&pool, &other.id, "acc_user_2", now).await.unwrap());

        // A row whose provider does not match the path stays benched too.
        seed_account(&pool, &user.id, "acc_grok", "grok", 7, account_meta("g@example.com")).await;
        crate::pool::bench::mark_benched(&pool, &user.id, "grok", "acc_grok", None, None, now).await.unwrap();
        let mismatched =
            call(&state, request("POST", "/api/providers/claude-code/accounts/acc_grok/unpause", Some(&cookie), None)).await;
        assert_eq!(mismatched.status(), StatusCode::NOT_FOUND);
        assert!(crate::pool::bench::is_benched(&pool, &user.id, "acc_grok", now).await.unwrap());
    }

    #[tokio::test]
    async fn unpause_nulls_the_bench_columns_and_is_idempotent() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        let now = state.now_ms();
        seed_account(&pool, &user.id, "acc_1", "claude-code", 7, account_meta("stored@example.com")).await;
        crate::pool::bench::mark_benched(&pool, &user.id, "claude-code", "acc_1", None, None, now).await.unwrap();
        assert!(crate::pool::bench::is_benched(&pool, &user.id, "acc_1", now).await.unwrap());

        let uri = "/api/providers/claude-code/accounts/acc_1/unpause";
        let res = call(&state, request("POST", uri, Some(&cookie), None)).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({ "ok": true }));
        assert!(!crate::pool::bench::is_benched(&pool, &user.id, "acc_1", now).await.unwrap());
        let row = stored_row(&pool, &user.id, "acc_1").await;
        assert!(row.bench_until.is_none() && row.bench_reason.is_none());

        stub_claude_usage(&transport, 1, "stored@example.com");
        let listed = claude_accounts(&state, &cookie, false).await;
        assert_ne!(listed["accounts"][0]["status"], "benched");

        // Unpausing an account that is not benched is still a 200.
        let again = call(&state, request("POST", uri, Some(&cookie), None)).await;
        assert_eq!(again.status(), StatusCode::OK);
        assert_eq!(body_json(again).await, json!({ "ok": true }));
    }

    // --------------------------------- the Fable route on the Claude Code status dot

    const TEAM_STANDARD: &str = "default_raven";
    const TEAM_PREMIUM: &str = "default_claude_max_5x";

    fn plan(plan_type: &str, tier: &str) -> Value {
        json!({ "email": "seat@example.com", "plan_type": plan_type, "rate_limit_tier": tier })
    }

    /// The profile stub carries no organization, so the seeded plan facts are what the route
    /// reads — the shape of a cached snapshot in production.
    async fn fable_statuses(state: &AppState, transport: &Arc<MockTransport>, cookie: &str, rows: usize) -> Vec<(String, String)> {
        stub_claude_usage(transport, rows, "upstream@example.com");
        let json = claude_accounts(state, cookie, false).await;
        json["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| (a["id"].as_str().unwrap().to_string(), a["status"].as_str().unwrap().to_string()))
            .collect()
    }

    fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
        rows.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[tokio::test]
    async fn a_first_seat_without_fable_reads_active_no_fable_and_the_next_eligible_one_active_fable() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_team", "claude-code", 15, plan("claude_team", TEAM_STANDARD)).await;
        seed_account(&pool, &user.id, "acc_max_a", "claude-code", 14, plan("claude_max", TEAM_PREMIUM)).await;
        seed_account(&pool, &user.id, "acc_max_b", "claude-code", 11, plan("claude_max", TEAM_PREMIUM)).await;

        assert_eq!(
            fable_statuses(&state, &transport, &cookie, 3).await,
            pairs(&[("acc_team", "active_no_fable"), ("acc_max_a", "active_fable"), ("acc_max_b", "standby")])
        );
    }

    #[tokio::test]
    async fn a_first_seat_with_fable_is_plain_active_and_nothing_below_it_is_active_fable() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_premium", "claude-code", 15, plan("claude_team", TEAM_PREMIUM)).await;
        seed_account(&pool, &user.id, "acc_max", "claude-code", 14, plan("claude_max", TEAM_PREMIUM)).await;

        assert_eq!(
            fable_statuses(&state, &transport, &cookie, 2).await,
            pairs(&[("acc_premium", "active"), ("acc_max", "standby")])
        );
    }

    #[tokio::test]
    async fn a_paused_fable_seat_never_takes_active_fable() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_team", "claude-code", 15, plan("claude_team", TEAM_STANDARD)).await;
        seed_account(&pool, &user.id, "acc_max_a", "claude-code", 14, plan("claude_max", TEAM_PREMIUM)).await;
        seed_account(&pool, &user.id, "acc_max_b", "claude-code", 11, plan("claude_max", TEAM_PREMIUM)).await;
        crate::pool::bench::mark_benched(&pool, &user.id, "claude-code", "acc_max_a", Some(60_000), Some("429"), state.now_ms())
            .await
            .unwrap();

        assert_eq!(
            fable_statuses(&state, &transport, &cookie, 3).await,
            pairs(&[("acc_team", "active_no_fable"), ("acc_max_a", "benched"), ("acc_max_b", "active_fable")])
        );
    }

    #[tokio::test]
    async fn a_pool_with_no_fable_seat_shows_active_no_fable_and_no_purple_row() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_team_a", "claude-code", 15, plan("claude_team", TEAM_STANDARD)).await;
        seed_account(&pool, &user.id, "acc_team_b", "claude-code", 14, plan("claude_team", TEAM_STANDARD)).await;

        assert_eq!(
            fable_statuses(&state, &transport, &cookie, 2).await,
            pairs(&[("acc_team_a", "active_no_fable"), ("acc_team_b", "standby")])
        );
    }

    #[tokio::test]
    async fn a_seat_with_no_stored_plan_facts_fails_open_to_plain_active() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "acc_fresh", "claude-code", 15, account_meta("stored@example.com")).await;

        assert_eq!(fable_statuses(&state, &transport, &cookie, 1).await, pairs(&[("acc_fresh", "active")]));
    }

    // --------------------------------------------------------------- Antigravity login

    const AG_CLIENT_ID: &str = "test-client.apps.googleusercontent.com";
    const AG_CLIENT_SECRET: &str = "test-client-secret";

    /// The client pair is never committed, so every test that reaches OAuth supplies one.
    fn antigravity_state(pool: PgPool, transport: Arc<MockTransport>, configured: bool) -> AppState {
        let mut config = test_config();
        if configured {
            config.antigravity_oauth_client_id = Some(AG_CLIENT_ID.into());
            config.antigravity_oauth_client_secret = Some(AG_CLIENT_SECRET.into());
        }
        AppState::builder(config, pool).transport(transport).build()
    }

    async fn start_antigravity_login(state: &AppState, cookie: &str) -> (String, String) {
        let res = call(state, request("POST", "/api/providers/antigravity/login", Some(cookie), None)).await;
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        let url = url::Url::parse(json["authorization_url"].as_str().unwrap()).unwrap();
        let oauth_state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.into_owned();
        (json["login_id"].as_str().unwrap().to_string(), oauth_state)
    }

    #[tokio::test]
    async fn antigravity_login_stores_the_pending_state_and_returns_the_authorize_url() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = antigravity_state(pool.clone(), MockTransport::new(), true);
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;

        let (_, oauth_state) = start_antigravity_login(&state, &cookie).await;
        let row: (String, String) =
            sqlx::query_as("SELECT provider, payload_json FROM oauth_login_states WHERE user_id = $1")
                .bind(&user.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0, "antigravity");
        let payload: Value = serde_json::from_str(&row.1).unwrap();
        assert_eq!(payload["client_id"], AG_CLIENT_ID);
        assert_eq!(payload["oauth_state"], oauth_state);
    }

    #[tokio::test]
    async fn antigravity_login_refuses_plainly_when_the_deploy_has_no_client() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = antigravity_state(pool.clone(), MockTransport::new(), false);
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;

        let res = call(&state, request("POST", "/api/providers/antigravity/login", Some(&cookie), None)).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(res).await["error"].as_str().unwrap().contains("ANTIGRAVITY_OAUTH_CLIENT_ID"));
        // No half-started login row is left behind.
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM oauth_login_states").fetch_one(&pool).await.unwrap();
        assert_eq!(rows, 0);

        // And the route requires a session.
        let anon = call(&state, request("POST", "/api/providers/antigravity/login", None, None)).await;
        assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn antigravity_complete_exchanges_the_code_resolves_the_project_and_encrypts_the_credential() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = antigravity_state(pool.clone(), transport.clone(), true);
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        let (login_id, oauth_state) = start_antigravity_login(&state, &cookie).await;

        for _ in 0..3 {
            transport.expect(|req| {
                if req.url == AntigravityOAuth::TOKEN_URL {
                    Ok(UpstreamResponse::json(
                        StatusCode::OK,
                        &json!({ "access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600 }),
                    ))
                } else if req.url.contains("userinfo") {
                    Ok(UpstreamResponse::json(StatusCode::OK, &json!({ "email": "a@b.com", "name": "A B" })))
                } else if req.url.contains("loadCodeAssist") {
                    Ok(UpstreamResponse::json(
                        StatusCode::OK,
                        &json!({ "cloudaicompanionProject": "proj-42", "allowedTiers": [{ "id": "pro-tier", "isDefault": true }] }),
                    ))
                } else {
                    panic!("unexpected upstream request {}", req.url)
                }
            });
        }

        let res = call(
            &state,
            request(
                "POST",
                &format!("/api/providers/antigravity/login/{login_id}/complete"),
                Some(&cookie),
                Some(json!({ "code": format!("http://localhost:51121/oauth-callback?code=code-1&state={oauth_state}") })),
            ),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["ok"], true);
        assert_eq!(json["label"], "a@b.com");

        let rows = list_accounts(&pool, &user.id, "antigravity").await.unwrap();
        assert_eq!(rows.len(), 1);
        let credential: StoredCredential =
            crate::crypto::token_crypto::decrypt_json(Some(TEST_TOKEN_KEY), &rows[0].encrypted_payload).unwrap();
        assert_eq!(credential.access_token, "at-1");
        assert_eq!(credential.refresh_token.as_deref(), Some("rt-1"));
        assert_eq!(credential.email.as_deref(), Some("a@b.com"));
        assert_eq!(credential.token_endpoint.as_deref(), Some(AntigravityOAuth::TOKEN_URL));
        // Project id rides inside the encrypted payload — no new column.
        let extra = credential.extra.unwrap();
        assert_eq!(extra["project_id"], "proj-42");
        assert_eq!(extra["tier_id"], "pro-tier");
        // The pending state is consumed, not left behind for replay.
        let states: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM oauth_login_states").fetch_one(&pool).await.unwrap();
        assert_eq!(states, 0);
    }

    #[tokio::test]
    async fn antigravity_complete_still_binds_the_account_when_the_project_bootstrap_fails() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = antigravity_state(pool.clone(), transport.clone(), true);
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        let (login_id, oauth_state) = start_antigravity_login(&state, &cookie).await;

        for _ in 0..6 {
            transport.expect(|req| {
                if req.url == AntigravityOAuth::TOKEN_URL {
                    Ok(UpstreamResponse::json(StatusCode::OK, &json!({ "access_token": "at-1" })))
                } else if req.url.contains("userinfo") {
                    Ok(UpstreamResponse::json(StatusCode::OK, &json!({ "email": "a@b.com" })))
                } else {
                    Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, Default::default(), "nope"))
                }
            });
        }

        let res = call(
            &state,
            request(
                "POST",
                &format!("/api/providers/antigravity/login/{login_id}/complete"),
                Some(&cookie),
                Some(json!({ "code": format!("x?code=1&state={oauth_state}") })),
            ),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let rows = list_accounts(&pool, &user.id, "antigravity").await.unwrap();
        let credential: StoredCredential =
            crate::crypto::token_crypto::decrypt_json(Some(TEST_TOKEN_KEY), &rows[0].encrypted_payload).unwrap();
        assert_eq!(credential.access_token, "at-1");
        assert!(credential.extra.is_none());
    }

    #[tokio::test]
    async fn antigravity_complete_rejects_a_forged_state_before_any_token_is_requested() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = antigravity_state(pool.clone(), transport.clone(), true);
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        let (login_id, _) = start_antigravity_login(&state, &cookie).await;

        let res = call(
            &state,
            request(
                "POST",
                &format!("/api/providers/antigravity/login/{login_id}/complete"),
                Some(&cookie),
                Some(json!({ "code": "http://localhost:51121/oauth-callback?code=code-1&state=forged" })),
            ),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        // The CSRF check must happen before any token is requested.
        assert!(transport.requests().is_empty());
        assert!(list_accounts(&pool, &user.id, "antigravity").await.unwrap().is_empty());
    }

    // ------------------------------------------------ the pool extension's borrower view

    fn share_info() -> ShareInfo {
        ShareInfo { team_id: "team_1".into(), team_name: "Acme".into(), owner_label: "owner@example.com".into() }
    }

    fn borrowed_row(id: &str, owner: &str) -> AccountRow {
        AccountRow {
            id: id.into(),
            user_id: owner.into(),
            provider: "grok".into(),
            external_account_id: None,
            label: Some(id.into()),
            custom_label: None,
            priority: 1,
            encrypted_payload: encrypt_json(
                Some(TEST_TOKEN_KEY),
                &StoredCredential { access_token: format!("token-{id}"), ..Default::default() },
            )
            .unwrap(),
            account_meta_json: None,
            usage_snapshot_json: None,
            usage_fetched_at: None,
            usage_fetching_at: None,
            bench_until: None,
            bench_reason: None,
            refreshing_at: None,
            edge_strikes: 0,
            edge_strike_at: None,
            created_at: NOW.into(),
            updated_at: NOW.into(),
        }
    }

    /// A `PoolExtension` whose calls are recorded; every method defaults to "nothing shared".
    #[derive(Default)]
    struct FakeExtension {
        shared: Vec<SharedAccount>,
        shared_with_usage: Option<Vec<SharedAccount>>,
        bars: Vec<UsageWindow>,
        set_shared_priority_ok: bool,
        calls: Mutex<Vec<(String, String, i32)>>,
    }

    #[async_trait]
    impl PoolExtension for FakeExtension {
        async fn list_shared(
            &self,
            _cx: &AppState,
            viewer_user_id: &str,
            provider: ProviderId,
            options: ListSharedOptions,
        ) -> anyhow::Result<Vec<SharedAccount>> {
            self.calls.lock().unwrap().push(("list_shared".into(), viewer_user_id.into(), options.usage as i32));
            if provider != ProviderId::Grok {
                return Ok(Vec::new());
            }
            if options.usage {
                if let Some(with_usage) = &self.shared_with_usage {
                    return Ok(with_usage.clone());
                }
            }
            Ok(self.shared.clone())
        }
        async fn reserve_attempt(
            &self,
            _cx: &AppState,
            _ctx: ReserveContext<'_>,
            _candidate: &RoutingCandidate,
        ) -> anyhow::Result<ReserveOutcome> {
            Ok(ReserveOutcome::Ungoverned)
        }
        async fn set_shared_priority(
            &self,
            _cx: &AppState,
            viewer_user_id: &str,
            account_id: &str,
            priority: i32,
        ) -> anyhow::Result<bool> {
            self.calls.lock().unwrap().push((format!("set_shared_priority:{account_id}"), viewer_user_id.into(), priority));
            Ok(self.set_shared_priority_ok)
        }
        async fn own_bars(
            &self,
            _cx: &AppState,
            viewer_user_id: &str,
            _provider: ProviderId,
            account_ids: &[String],
        ) -> anyhow::Result<HashMap<String, Vec<UsageWindow>>> {
            self.calls.lock().unwrap().push((format!("own_bars:{}", account_ids.join(",")), viewer_user_id.into(), 0));
            Ok(account_ids.iter().map(|id| (id.clone(), self.bars.clone())).collect())
        }
    }

    fn state_with_extension(pool: PgPool, transport: Arc<MockTransport>, ext: Arc<FakeExtension>) -> AppState {
        AppState::builder(test_config(), pool).transport(transport).pool_extension(Some(ext)).build()
    }

    async fn grok_accounts(state: &AppState, cookie: &str) -> Value {
        body_json(call(state, request("GET", "/api/providers/grok/accounts", Some(cookie), None)).await).await
    }

    #[tokio::test]
    async fn a_shared_row_is_listed_with_its_share_descriptor_and_no_usage_surface() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let mut row = borrowed_row("shared_1", "user_2");
        row.label = Some("owner-seat".into());
        let ext = Arc::new(FakeExtension {
            shared: vec![SharedAccount { account: row, priority: 3, share: share_info(), usage: None }],
            set_shared_priority_ok: true,
            ..Default::default()
        });
        let state = state_with_extension(pool.clone(), transport.clone(), ext);
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;
        // A borrower must never trigger the owner's usage probe: the transport has no handler,
        // so any request would panic.

        let json = grok_accounts(&state, &cookie).await;
        assert_eq!(json["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(json["accounts"][0]["id"], "shared_1");
        assert_eq!(json["accounts"][0]["priority"], 3);
        assert_eq!(json["accounts"][0]["label"], "owner-seat");
        assert_eq!(json["accounts"][0]["usage"], Value::Null);
        assert_eq!(json["accounts"][0]["error"], Value::Null);
        assert_eq!(json["accounts"][0]["account"], Value::Null);
        assert_eq!(
            json["accounts"][0]["share"],
            json!({ "teamId": "team_1", "teamName": "Acme", "ownerLabel": "owner@example.com" })
        );
        assert!(!json.to_string().contains("token-shared_1"));
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn the_extensions_own_bars_for_a_shared_row_are_passed_through_and_asked_for() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let windows = vec![UsageWindow {
            label: "Team · 100 requests / month".into(),
            utilization: Some(42.0),
            resets_at: None,
            value: None,
        }];
        let ext = Arc::new(FakeExtension {
            shared_with_usage: Some(vec![SharedAccount {
                account: borrowed_row("shared_1", "user_2"),
                priority: 3,
                share: share_info(),
                usage: Some(AccountUsage { windows: windows.clone() }),
            }]),
            set_shared_priority_ok: true,
            ..Default::default()
        });
        let recorder = ext.clone();
        let state = state_with_extension(pool.clone(), transport.clone(), ext);
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;

        let json = grok_accounts(&state, &cookie).await;
        assert_eq!(json["accounts"][0]["id"], "shared_1");
        assert_eq!(json["accounts"][0]["usage"]["windows"][0]["label"], "Team · 100 requests / month");
        assert_eq!(json["accounts"][0]["usage"]["windows"][0]["utilization"], 42.0);
        assert!(recorder
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(name, viewer, usage)| name == "list_shared" && viewer == &user.id && *usage == 1));
    }

    #[tokio::test]
    async fn own_bars_are_placed_before_the_upstream_windows_without_touching_the_dot() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        // The viewer's own usage probe is allowed to fail; it must not decide the dot.
        for _ in 0..6 {
            transport.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, Default::default(), "upstream down")));
        }
        let bar = UsageWindow {
            label: "Request".into(),
            utilization: Some(100.0),
            resets_at: Some("2999-01-01T00:00:00.000Z".into()),
            value: Some("100 / 100".into()),
        };
        let ext = Arc::new(FakeExtension { bars: vec![bar.clone()], set_shared_priority_ok: true, ..Default::default() });
        let recorder = ext.clone();
        let state = state_with_extension(pool.clone(), transport.clone(), ext);
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "own_1", "grok", 1, account_meta("own@example.com")).await;

        let json = grok_accounts(&state, &cookie).await;
        assert!(recorder
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(name, viewer, _)| name == "own_bars:own_1" && viewer == &user.id));
        let windows = json["accounts"][0]["usage"]["windows"].as_array().unwrap();
        assert_eq!(windows[0]["label"], "Request");
        assert_eq!(windows[0]["value"], "100 / 100");
        // A full allowance bar is the edition's business; the routing dot still reads active.
        assert_eq!(json["accounts"][0]["status"], "active");
    }

    #[tokio::test]
    async fn the_list_follows_the_routers_merged_order_with_a_promoted_shared_row_first() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        for _ in 0..6 {
            transport.expect(|req| {
                assert!(!req.url.contains("shared_1"), "shared rows are never probed");
                Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, Default::default(), "upstream down"))
            });
        }
        let ext = Arc::new(FakeExtension {
            shared: vec![SharedAccount {
                account: borrowed_row("shared_1", "user_2"),
                priority: 9,
                share: share_info(),
                usage: None,
            }],
            set_shared_priority_ok: true,
            ..Default::default()
        });
        let state = state_with_extension(pool.clone(), transport.clone(), ext);
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "own_1", "grok", 1, account_meta("own@example.com")).await;

        let json = grok_accounts(&state, &cookie).await;
        let rows: Vec<(&str, &str)> = json["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| (a["id"].as_str().unwrap(), a["status"].as_str().unwrap()))
            .collect();
        assert_eq!(rows, vec![("shared_1", "active"), ("own_1", "standby")]);
    }

    // ----------------------------------------------------------- mutations on a borrowed row

    fn borrowing_extension(ok: bool) -> Arc<FakeExtension> {
        Arc::new(FakeExtension {
            shared: vec![SharedAccount {
                account: borrowed_row("shared_1", "user_2"),
                priority: 3,
                share: share_info(),
                usage: None,
            }],
            set_shared_priority_ok: ok,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn every_owner_only_mutation_on_a_borrowed_row_is_403_not_404() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = state_with_extension(pool.clone(), MockTransport::new(), borrowing_extension(true));
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;

        for (method, path, body) in [
            ("PATCH", "/api/providers/grok/accounts/shared_1", Some(json!({ "custom_label": "mine" }))),
            ("POST", "/api/providers/grok/accounts/shared_1/unpause", None),
            ("DELETE", "/api/providers/grok/accounts/shared_1", None),
        ] {
            let res = call(&state, request(method, path, Some(&cookie), body)).await;
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{method} {path}");
            assert_eq!(body_json(res).await, json!({ "error": "forbidden" }));
        }

        // An id that is neither owned nor shared is still 404.
        let ghost = call(&state, request("DELETE", "/api/providers/grok/accounts/ghost", Some(&cookie), None)).await;
        assert_eq!(ghost.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn promote_delegates_to_set_shared_priority_one_above_the_merged_maximum() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let ext = borrowing_extension(true);
        let recorder = ext.clone();
        let state = state_with_extension(pool.clone(), MockTransport::new(), ext);
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        seed_account(&pool, &user.id, "own_1", "grok", 9, account_meta("own@example.com")).await;

        let res = call(&state, request("POST", "/api/providers/grok/accounts/shared_1/promote", Some(&cookie), None)).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({ "ok": true }));
        assert!(recorder.calls.lock().unwrap().iter().any(|(name, viewer, priority)| {
            name == "set_shared_priority:shared_1" && viewer == &user.id && *priority == 10
        }));
        // The owner's row is never rewritten by a borrower's promote.
        let ids: Vec<String> =
            list_accounts(&pool, &user.id, "grok").await.unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["own_1".to_string()]);
    }

    #[tokio::test]
    async fn promote_is_404_when_the_extension_refuses() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = state_with_extension(pool.clone(), MockTransport::new(), borrowing_extension(false));
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;

        let res = call(&state, request("POST", "/api/providers/grok/accounts/shared_1/promote", Some(&cookie), None)).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_standalone_install_sees_no_shared_rows_at_all() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;
        assert_eq!(grok_accounts(&state, &cookie).await["accounts"].as_array().unwrap().len(), 0);
    }

    // ---------------------------------------------------- POST /:provider/accounts/import

    #[tokio::test]
    async fn import_binds_an_encrypted_credential_and_never_echoes_it() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;

        let missing = call(
            &state,
            request("POST", "/api/providers/grok/accounts/import", Some(&cookie), Some(json!({ "email": "a@b.c" }))),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(missing).await, json!({ "error": "access_token required" }));

        let res = call(
            &state,
            request(
                "POST",
                "/api/providers/grok/accounts/import",
                Some(&cookie),
                Some(json!({ "access_token": "imported-token", "email": "a@b.c", "account_id": "acct-9" })),
            ),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["ok"], true);
        assert!(!json.to_string().contains("imported-token"));

        let rows = list_accounts(&pool, &user.id, "grok").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].external_account_id.as_deref(), Some("acct-9"));
        assert_eq!(rows[0].label.as_deref(), Some("a@b.c"));
        let credential: StoredCredential =
            crate::crypto::token_crypto::decrypt_json(Some(TEST_TOKEN_KEY), &rows[0].encrypted_payload).unwrap();
        assert_eq!(credential.access_token, "imported-token");
    }
}
