//! `/api/model-groups` — the Groups page's CRUD (docs/api.md § Model groups, docs/providers.md
//! § Model groups, docs/auth.md § Model groups).
//!
//! A group is a slug-addressed endpoint (`/g/<slug>/…`) carrying 1..20 callable models, each
//! with its own ordered target list. Targets are validated against the caller's own providers
//! and accounts, plus what a pool extension shares with them — never anyone else's — and
//! the read shape enriches each target with a read-time account label and the stored-state
//! routing indicator the page draws.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use crate::auth::session::SessionUser;
use crate::db::accounts::get_account;
use crate::db::model_groups::{
    delete_model_group, get_model_group_by_id, insert_model_group, list_model_groups,
    list_models_for_group, parse_group_targets, replace_group_models, update_model_group_fields, GroupModelInput,
    GroupTarget, ModelGroupRow,
};
use crate::http::errors::{admin_error as error, internal_error};
use crate::providers::ProviderId;
use crate::routing::candidates::{candidates_for_target, own_provider_slugs, pinned_account, resolve_target_prefix, SharedLookup};
use crate::routing::facts::{candidate_facts_list, earliest_unusable_until};
use crate::routing::strategy::DEFAULT_STRATEGY;
use crate::routing::types::CandidateFacts;
use crate::utils::model::split_model_id;
use crate::utils::model_group::{validate_display_name, validate_group_models, validate_group_slug, validate_strategy, MAX_MODEL_GROUPS_PER_USER};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(list).post(create)).route("/{id}", put(update).delete(remove))
}

/// An absent or unparsable body is `400 {"error":"invalid JSON"}`. A body that parses but is
/// not an object reads as `{}`, so its missing fields fail the field validators instead. The
/// bytes are parsed here rather than through the `Json` extractor so the envelope stays the
/// route's, not the framework's.
fn object_body(body: &Bytes) -> Option<Map<String, Value>> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Some(map),
        Ok(_) => Some(Map::new()),
        Err(_) => None,
    }
}

/// Read-time display label for a pinned target — `custom_label` wins over the upstream
/// `label`, `None` when unpinned or the account no longer resolves (deleted, or no longer
/// shared with the caller). The row is the one routing would pin, so the label and the
/// routing indicator never disagree. Never stored.
async fn account_label(state: &AppState, user_id: &str, lookup: &SharedLookup, target: &GroupTarget) -> Result<Option<String>, sqlx::Error> {
    let Some(account_id) = target.account_id.as_deref() else { return Ok(None) };
    let Some(split) = split_model_id(&target.model) else { return Ok(None) };
    let Some((row, _)) = pinned_account(state, user_id, lookup, &split.prefix, account_id).await? else { return Ok(None) };
    Ok(row.custom_label.filter(|s| !s.is_empty()).or(row.label.filter(|s| !s.is_empty())))
}

/// A bench that lasts at least as long as the usage window is the effective blocker. This
/// also makes equal expiries deterministic.
fn reason_for_facts(facts: &CandidateFacts) -> &'static str {
    match facts.bench_until {
        Some(bench) if facts.usage_window_until.is_none_or(|window| bench > window) => "benched",
        _ => "limit",
    }
}

/// One target's stored-state routing: what the ordered walk would do right now, from stored
/// facts only — no upstream call (docs/providers.md § Routing module).
async fn routing_for_target(
    state: &AppState,
    user_id: &str,
    lookup: &SharedLookup,
    target: &GroupTarget,
) -> anyhow::Result<Value> {
    let Some(resolved) = resolve_target_prefix(state, user_id, lookup, target).await? else {
        return Ok(json!({ "usable": false, "reason": "unresolved", "unusable_until": Value::Null }));
    };
    let candidates = candidates_for_target(state, user_id, lookup, &resolved).await?;
    if candidates.is_empty() {
        return Ok(json!({ "usable": false, "reason": "no_account", "unusable_until": Value::Null }));
    }
    let facts = candidate_facts_list(&candidates, state.now_ms());
    if facts.iter().any(|f| f.usable) {
        return Ok(json!({ "usable": true, "reason": Value::Null, "unusable_until": Value::Null }));
    }
    let until_ms = earliest_unusable_until(&facts);
    let blocking = facts.iter().find(|f| f.unusable_until == until_ms).expect("a blocking fact");
    Ok(json!({
        "usable": false,
        "reason": reason_for_facts(blocking),
        "unusable_until": until_ms.map(crate::db::accounts::iso_from_ms),
    }))
}

/// One group model's read shape: name, enriched targets, per-model routing.
async fn to_model_item(
    state: &AppState,
    user_id: &str,
    lookup: &SharedLookup,
    name: &str,
    targets: &[GroupTarget],
) -> anyhow::Result<Value> {
    let mut enriched = Vec::with_capacity(targets.len());
    for target in targets {
        enriched.push(json!({
            "model": target.model,
            "account_id": target.account_id,
            "account_label": account_label(state, user_id, lookup, target).await?,
        }));
    }
    let mut routing_targets = Vec::with_capacity(targets.len());
    for target in targets {
        routing_targets.push(routing_for_target(state, user_id, lookup, target).await?);
    }
    let current_target_index = routing_targets.iter().position(|t| t["usable"] == Value::Bool(true));
    Ok(json!({
        "name": name,
        "targets": enriched,
        // The current-route indicator, per model: what the ordered walk would dispatch right
        // now, from stored facts only.
        "routing": {
            "current_target_index": current_target_index,
            "targets": routing_targets,
        },
    }))
}

/// `lookup` spans one whole response, so a list of many groups asks the extension once at most.
async fn to_list_item(state: &AppState, user_id: &str, lookup: &SharedLookup, row: &ModelGroupRow) -> anyhow::Result<Value> {
    let model_rows = list_models_for_group(state.pool(), &row.id).await?;
    let mut models = Vec::with_capacity(model_rows.len());
    for model in &model_rows {
        let targets = parse_group_targets(Some(&model.targets_json));
        models.push(to_model_item(state, user_id, lookup, &model.name, &targets).await?);
    }
    Ok(json!({
        "id": row.id,
        "name": row.name,
        "slug": row.slug,
        "models": models,
        "strategy": row.strategy,
        "updated_at": row.updated_at,
    }))
}

/// Validates `models`. `Err(message)` is the 400 body's text. A target's prefix is a builtin
/// `ProviderId`, one of the caller's own custom or CLI provider slugs (docs/cli.md), or the
/// slug of a custom endpoint a pool extension shares with the caller, never another user's
/// otherwise. A pinned `account_id` is the caller's own row or, for a builtin prefix, a row the
/// pool extension currently shares with the caller — never a shared endpoint's key row, since a
/// shared slug is not a builtin. A target equal to one in `stored` (the group's own, on an
/// update) is kept without either check.
async fn validate_models(
    state: &AppState,
    user_id: &str,
    models: &Value,
    stored: &[GroupTarget],
) -> anyhow::Result<Result<Vec<GroupModelInput>, String>> {
    let own = own_provider_slugs(state, user_id).await?;
    let missed_own = AtomicBool::new(false);
    let own_prefix = |prefix: &str| {
        let known = ProviderId::parse(prefix).is_some() || own.contains(prefix);
        if !known {
            missed_own.store(true, Ordering::Relaxed);
        }
        known
    };
    let lookup = SharedLookup::default();
    // A storage or extension error while checking a pin. The validator only takes a yes/no, so
    // the error is parked here and wins over its verdict: a 500, never a misleading 400.
    let failure: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    // A pinned `account_id` must match the target's prefix (docs/auth.md § Model groups) and be
    // the caller's own row or one shared with them — never a row that quietly belongs to a
    // different provider than the target claims. An own row of that id always decides.
    let resolve_account = |account_id: String, provider: String| {
        let (lookup, failure) = (&lookup, &failure);
        async move {
            let fail = |error: anyhow::Error| {
                failure.lock().expect("failure slot").get_or_insert(error);
                false
            };
            match get_account(state.pool(), user_id, &account_id).await {
                Ok(Some(row)) => return row.provider == provider,
                Ok(None) => {}
                Err(error) => return fail(error.into()),
            }
            let Some(builtin) = ProviderId::parse(&provider) else { return false };
            match lookup.shared_account(state, user_id, builtin, &account_id).await {
                Ok(shared) => shared.is_some(),
                Err(error) => fail(error),
            }
        }
    };
    let take_failure = || failure.lock().expect("failure slot").take();
    let mut validated = validate_group_models(models, &own_prefix, &resolve_account, stored).await;
    if let Some(error) = take_failure() {
        return Err(error);
    }
    // Only a prefix the caller's own namespace missed asks the extension, so a lost sharing
    // store never blocks a group made of the caller's own providers. When it is asked, its
    // error propagates: a 500, never a misleading 400 on a valid shared slug.
    if validated.is_err() && missed_own.load(Ordering::Relaxed) {
        let shared: HashSet<String> = lookup.all(state, user_id).await?.iter().map(|s| s.provider.slug.clone()).collect();
        let any_prefix = |prefix: &str| ProviderId::parse(prefix).is_some() || own.contains(prefix) || shared.contains(prefix);
        validated = validate_group_models(models, &any_prefix, &resolve_account, stored).await;
        if let Some(error) = take_failure() {
            return Err(error);
        }
    }
    Ok(validated)
}

async fn list(State(state): State<AppState>, session: SessionUser) -> Response {
    list_for_user(&state, &session.user.id).await
}

/// The list body, keyed by user — the session route and `/cli/v1/model-groups` share it
/// (docs/cli.md § Server routes: one validator, one response shape).
pub(crate) async fn list_for_user(state: &AppState, user_id: &str) -> Response {
    let Ok(rows) = list_model_groups(state.pool(), user_id).await else { return internal_error() };
    let lookup = SharedLookup::default();
    let mut groups = Vec::with_capacity(rows.len());
    for row in &rows {
        match to_list_item(state, user_id, &lookup, row).await {
            Ok(item) => groups.push(item),
            Err(_) => return internal_error(),
        }
    }
    Json(json!({ "groups": groups })).into_response()
}

async fn create(State(state): State<AppState>, session: SessionUser, body: Bytes) -> Response {
    create_for_user(&state, &session.user.id, &body).await
}

pub(crate) async fn create_for_user(state: &AppState, user_id: &str, body: &Bytes) -> Response {
    let Some(body) = object_body(body) else { return error(StatusCode::BAD_REQUEST, "invalid JSON") };

    let name = body.get("name").and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    if let Some(message) = validate_display_name(&name) {
        return error(StatusCode::BAD_REQUEST, &message);
    }
    let slug = body.get("slug").and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    if let Some(message) = validate_group_slug(&slug) {
        return error(StatusCode::BAD_REQUEST, &message);
    }

    let models_value = body.get("models").cloned().unwrap_or(Value::Null);
    let models = match validate_models(state, user_id, &models_value, &[]).await {
        Err(_) => return internal_error(),
        Ok(Err(message)) => return error(StatusCode::BAD_REQUEST, &message),
        Ok(Ok(models)) => models,
    };

    // `strategy` defaults to `ordered`; only `ordered` is accepted today.
    let strategy = body.get("strategy").cloned().unwrap_or_else(|| Value::String(DEFAULT_STRATEGY.to_string()));
    if let Some(message) = validate_strategy(&strategy) {
        return error(StatusCode::BAD_REQUEST, &message);
    }
    let strategy = strategy.as_str().unwrap_or(DEFAULT_STRATEGY).to_string();

    let Ok(existing) = list_model_groups(state.pool(), user_id).await else { return internal_error() };
    if existing.len() >= MAX_MODEL_GROUPS_PER_USER {
        return error(StatusCode::BAD_REQUEST, &format!("maximum of {MAX_MODEL_GROUPS_PER_USER} model groups reached"));
    }
    if existing.iter().any(|g| g.name == name) {
        return error(StatusCode::BAD_REQUEST, &format!("a model group named \"{name}\" already exists"));
    }
    if existing.iter().any(|g| g.slug == slug) {
        return error(StatusCode::BAD_REQUEST, &format!("slug \"{slug}\" is already used by another of your groups"));
    }

    let Ok(row) = insert_model_group(state.pool(), user_id, &name, &slug, Some(&strategy)).await else {
        return internal_error();
    };
    if replace_group_models(state.pool(), user_id, &row.id, &models).await.is_err() {
        return internal_error();
    }
    match to_list_item(state, user_id, &SharedLookup::default(), &row).await {
        Ok(item) => (StatusCode::CREATED, Json(item)).into_response(),
        Err(_) => internal_error(),
    }
}

async fn update(
    State(state): State<AppState>,
    session: SessionUser,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    update_for_user(&state, &session.user.id, &id, &body).await
}

pub(crate) async fn update_for_user(state: &AppState, user_id: &str, id: &str, body: &Bytes) -> Response {
    let existing = match get_model_group_by_id(state.pool(), user_id, id).await {
        Err(_) => return internal_error(),
        Ok(None) => return error(StatusCode::NOT_FOUND, "not found"),
        Ok(Some(row)) => row,
    };
    let Some(body) = object_body(body) else { return error(StatusCode::BAD_REQUEST, "invalid JSON") };
    // The caller's other groups, read once for both uniqueness checks.
    let others: Vec<ModelGroupRow> = if body.contains_key("name") || body.contains_key("slug") {
        let Ok(rows) = list_model_groups(state.pool(), user_id).await else { return internal_error() };
        rows.into_iter().filter(|g| g.id != id).collect()
    } else {
        Vec::new()
    };

    let mut name: Option<String> = None;
    if let Some(raw) = body.get("name") {
        let candidate = raw.as_str().map(str::trim).unwrap_or("").to_string();
        if let Some(message) = validate_display_name(&candidate) {
            return error(StatusCode::BAD_REQUEST, &message);
        }
        if others.iter().any(|g| g.name == candidate) {
            return error(StatusCode::BAD_REQUEST, &format!("a model group named \"{candidate}\" already exists"));
        }
        name = Some(candidate);
    }

    let mut slug: Option<String> = None;
    if let Some(raw) = body.get("slug") {
        let candidate = raw.as_str().map(str::trim).unwrap_or("").to_string();
        if let Some(message) = validate_group_slug(&candidate) {
            return error(StatusCode::BAD_REQUEST, &message);
        }
        if others.iter().any(|g| g.slug == candidate) {
            return error(StatusCode::BAD_REQUEST, &format!("slug \"{candidate}\" is already used by another of your groups"));
        }
        slug = Some(candidate);
    }

    let mut models: Option<Vec<GroupModelInput>> = None;
    if let Some(raw) = body.get("models") {
        // The group's stored targets keep their pins even when they no longer resolve
        // (docs/providers.md § Model groups "Account pinning").
        let Ok(stored_models) = list_models_for_group(state.pool(), id).await else { return internal_error() };
        let stored: Vec<GroupTarget> =
            stored_models.iter().flat_map(|m| parse_group_targets(Some(&m.targets_json))).collect();
        match validate_models(state, user_id, raw, &stored).await {
            Err(_) => return internal_error(),
            Ok(Err(message)) => return error(StatusCode::BAD_REQUEST, &message),
            Ok(Ok(validated)) => models = Some(validated),
        }
    }

    let mut strategy: Option<String> = None;
    if let Some(raw) = body.get("strategy") {
        if let Some(message) = validate_strategy(raw) {
            return error(StatusCode::BAD_REQUEST, &message);
        }
        strategy = raw.as_str().map(str::to_string);
    }

    if update_model_group_fields(state.pool(), id, name.as_deref(), slug.as_deref(), strategy.as_deref()).await.is_err() {
        return internal_error();
    }
    if let Some(models) = models {
        if replace_group_models(state.pool(), user_id, id, &models).await.is_err() {
            return internal_error();
        }
    }
    let updated = match get_model_group_by_id(state.pool(), user_id, id).await {
        Err(_) => return internal_error(),
        Ok(row) => row.unwrap_or(existing),
    };
    match to_list_item(state, user_id, &SharedLookup::default(), &updated).await {
        Ok(item) => Json(item).into_response(),
        Err(_) => internal_error(),
    }
}

async fn remove(State(state): State<AppState>, session: SessionUser, Path(id): Path<String>) -> Response {
    remove_for_user(&state, &session.user.id, &id).await
}

pub(crate) async fn remove_for_user(state: &AppState, user_id: &str, id: &str) -> Response {
    match delete_model_group(state.pool(), user_id, id).await {
        Err(_) => internal_error(),
        Ok(false) => error(StatusCode::NOT_FOUND, "not found"),
        Ok(true) => Json(json!({ "ok": true })).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::session::create_session;
    use crate::db::accounts::iso_from_ms;
    use crate::db::custom_providers::{insert_custom_provider, NewCustomProvider};
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_router, test_state};
    use crate::upstream::MockTransport;
    use crate::pool::extension::test_support::{owner_endpoint, state_with_extension, SharedCustomExtension};
    use crate::utils::model_group::{MAX_MODELS_PER_GROUP, MAX_TARGETS_PER_MODEL};
    use axum::body::Body;
    use axum::http::{header, Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn signed_in(state: &AppState, email: &str) -> (String, String) {
        let user = insert_user(state.pool(), email).await;
        let (_, cookie) = create_session(state, &user.id, false).await.unwrap();
        (user.id, cookie.split(';').next().unwrap().to_string())
    }

    fn request(method: &str, uri: &str, cookie: &str, body: Option<Value>) -> Request<Body> {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/json");
        match body {
            Some(body) => builder.body(Body::from(body.to_string())).unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        }
    }

    async fn send(state: &AppState, method: &str, uri: &str, cookie: &str, body: Option<Value>) -> Response {
        test_router(state.clone()).oneshot(request(method, uri, cookie, body)).await.unwrap()
    }

    fn valid_create_body() -> Value {
        json!({
            "name": "Opus",
            "slug": "opus-ep",
            "models": [{ "name": "opus", "targets": ["claude-code/claude-opus-5"] }],
        })
    }

    async fn create_group(state: &AppState, cookie: &str, overrides: Value) -> Response {
        let mut body = valid_create_body();
        for (k, v) in overrides.as_object().expect("overrides object") {
            body[k] = v.clone();
        }
        send(state, "POST", "/api/model-groups", cookie, Some(body)).await
    }

    async fn seed_custom_provider(state: &AppState, user_id: &str, slug: &str) {
        insert_custom_provider(
            state.pool(),
            NewCustomProvider {
                user_id,
                slug,
                name: slug,
                format: "openai",
                base_url: "https://upstream.example.com/v1",
                count_tokens_url: None,
                models_mode: "auto",
                manual_models_json: None,
            },
        )
        .await
        .expect("insert custom provider")
        .expect("slug is free");
    }

    async fn seed_account(state: &AppState, user_id: &str, id: &str, provider: &str, label: Option<&str>, custom: Option<&str>) {
        sqlx::query(
            "INSERT INTO upstream_accounts (id, user_id, provider, external_account_id, label, custom_label, priority,
                                            encrypted_payload, created_at, updated_at)
             VALUES ($1, $2, $3, NULL, $4, $5, 1, 'encrypted', $6, $6)",
        )
        .bind(id)
        .bind(user_id)
        .bind(provider)
        .bind(label)
        .bind(custom)
        .bind(crate::ids::now_iso())
        .execute(state.pool())
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn every_route_requires_a_session() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        for (method, uri) in [
            ("GET", "/api/model-groups"),
            ("POST", "/api/model-groups"),
            ("PUT", "/api/model-groups/mgrp_1"),
            ("DELETE", "/api/model-groups/mgrp_1"),
        ] {
            let response = test_router(state.clone())
                .oneshot(Request::builder().method(method).uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn lists_the_callers_groups_with_slug_models_and_targets_in_order() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let (_, cookie) = signed_in(&state, "list@example.com").await;
        let (_, other_cookie) = signed_in(&state, "other@example.com").await;
        let response = create_group(
            &state,
            &cookie,
            json!({ "models": [
                { "name": "gpt-4o", "targets": ["claude-code/claude-opus-5", "grok/grok-4.5"] },
                { "name": "gpt-4", "targets": ["grok/grok-4.5"] },
            ] }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);

        let json = body_json(send(&state, "GET", "/api/model-groups", &cookie, None).await).await;
        let groups = json["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["name"], "Opus");
        assert_eq!(groups[0]["slug"], "opus-ep");
        assert_eq!(groups[0]["strategy"], "ordered", "GET lists strategy on every group");
        assert_eq!(groups[0]["models"][0]["name"], "gpt-4o");
        assert_eq!(
            groups[0]["models"][0]["targets"],
            json!([
                { "model": "claude-code/claude-opus-5", "account_id": null, "account_label": null },
                { "model": "grok/grok-4.5", "account_id": null, "account_label": null },
            ])
        );
        assert_eq!(groups[0]["models"][1]["name"], "gpt-4");

        // Another user sees none of it, and may reuse both the name and the slug.
        let json = body_json(send(&state, "GET", "/api/model-groups", &other_cookie, None).await).await;
        assert_eq!(json["groups"].as_array().unwrap().len(), 0);
        assert_eq!(create_group(&state, &other_cookie, json!({})).await.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn reports_stored_state_routing_per_target() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let (user_id, cookie) = signed_in(&state, "routing@example.com").await;
        seed_custom_provider(&state, &user_id, "deleted-endpoint").await;
        seed_custom_provider(&state, &user_id, "empty-endpoint").await;
        seed_account(&state, &user_id, "limited", "claude-code", None, None).await;
        seed_account(&state, &user_id, "healthy", "grok", None, None).await;
        seed_account(&state, &user_id, "pool-limited", "codex", None, None).await;
        seed_account(&state, &user_id, "pool-healthy", "codex", None, None).await;
        let reset = iso_from_ms(state.now_ms() + 60_000);
        for id in ["limited", "pool-limited"] {
            sqlx::query("UPDATE upstream_accounts SET usage_snapshot_json = $1 WHERE id = $2")
                .bind(json!({ "windows": [{ "utilization": 100, "resets_at": reset }], "error": null, "stale": false }).to_string())
                .bind(id)
                .execute(state.pool())
                .await
                .unwrap();
        }
        let created = body_json(
            create_group(
                &state,
                &cookie,
                json!({ "models": [{ "name": "the-model", "targets": [
                    { "model": "claude-code/first", "account_id": "limited" },
                    { "model": "grok/second", "account_id": "healthy" },
                    "deleted-endpoint/model",
                    "codex/pooled",
                ] }] }),
            )
            .await,
        )
        .await;
        let group_id = created["id"].as_str().unwrap().to_string();

        // Preserve formerly valid stored targets the write-time validator would reject today.
        sqlx::query("DELETE FROM custom_providers WHERE slug = 'deleted-endpoint'").execute(state.pool()).await.unwrap();
        sqlx::query("UPDATE model_group_models SET targets_json = $1 WHERE group_id = $2")
            .bind(
                json!([
                    { "model": "claude-code/first", "account_id": "limited" },
                    { "model": "grok/second", "account_id": "healthy" },
                    { "model": "deleted-endpoint/model", "account_id": null },
                    { "model": "claude-code/missing", "account_id": "gone" },
                    { "model": "codex/pooled", "account_id": null },
                    { "model": "empty-endpoint/no-pool", "account_id": null },
                ])
                .to_string(),
            )
            .bind(&group_id)
            .execute(state.pool())
            .await
            .unwrap();

        let json = body_json(send(&state, "GET", "/api/model-groups", &cookie, None).await).await;
        let routing = &json["groups"][0]["models"][0]["routing"];
        assert_eq!(routing["current_target_index"], 1);
        assert_eq!(
            routing["targets"],
            json!([
                { "usable": false, "reason": "limit", "unusable_until": reset },
                { "usable": true, "reason": null, "unusable_until": null },
                { "usable": false, "reason": "unresolved", "unusable_until": null },
                { "usable": false, "reason": "no_account", "unusable_until": null },
                { "usable": true, "reason": null, "unusable_until": null },
                { "usable": false, "reason": "no_account", "unusable_until": null },
            ])
        );
    }

    #[tokio::test]
    async fn a_longer_bench_reads_as_benched_when_both_apply() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let (user_id, cookie) = signed_in(&state, "bench@example.com").await;
        seed_account(&state, &user_id, "acc", "claude-code", None, None).await;
        let reset = state.now_ms() + 60_000;
        sqlx::query("UPDATE upstream_accounts SET usage_snapshot_json = $1, bench_until = $2 WHERE id = 'acc'")
            .bind(
                json!({ "windows": [{ "utilization": 100, "resets_at": iso_from_ms(reset) }], "error": null, "stale": false })
                    .to_string(),
            )
            .bind(iso_from_ms(reset + 60_000))
            .execute(state.pool())
            .await
            .unwrap();
        create_group(
            &state,
            &cookie,
            json!({ "models": [{ "name": "the-model", "targets": [{ "model": "claude-code/model", "account_id": "acc" }] }] }),
        )
        .await;

        let json = body_json(send(&state, "GET", "/api/model-groups", &cookie, None).await).await;
        let target = &json["groups"][0]["models"][0]["routing"]["targets"][0];
        assert_eq!(target["usable"], false);
        assert_eq!(target["reason"], "benched");
    }

    #[tokio::test]
    async fn create_returns_201_and_validates_name_and_slug() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let (_, cookie) = signed_in(&state, "create@example.com").await;

        let json = body_json(create_group(&state, &cookie, json!({})).await).await;
        assert_eq!(json["name"], "Opus");
        assert_eq!(json["slug"], "opus-ep");
        assert!(json["id"].is_string());
        assert_eq!(json["strategy"], "ordered", "POST without strategy defaults to ordered");
        assert_eq!(
            json["models"][0]["targets"],
            json!([{ "model": "claude-code/claude-opus-5", "account_id": null, "account_label": null }])
        );

        // The display name is free text — whitespace is fine, since it is never part of a URL.
        let response = create_group(&state, &cookie, json!({ "name": "OpenAI GPT-4o family", "slug": "family-ep" })).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(body_json(response).await["name"], "OpenAI GPT-4o family");

        for bad_name in [json!(""), json!("a".repeat(65))] {
            let response = create_group(&state, &cookie, json!({ "name": bad_name, "slug": "other-ep" })).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        for bad_slug in ["UPPER", "a", "-lead"] {
            let response = create_group(&state, &cookie, json!({ "name": "Other", "slug": bad_slug })).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad_slug}");
        }

        // A duplicate display name and a duplicate slug are each refused, the slug by name.
        let response = create_group(&state, &cookie, json!({ "slug": "opus-2" })).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = create_group(&state, &cookie, json!({ "name": "Other", "slug": "opus-ep" })).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(response).await["error"].as_str().unwrap().contains("opus-ep"));

        // A body that is not JSON at all is a 400 with the parse message.
        let response = test_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/model-groups")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await, json!({ "error": "invalid JSON" }));
    }

    #[tokio::test]
    async fn create_validates_the_model_set() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let (user_id, cookie) = signed_in(&state, "models@example.com").await;
        seed_custom_provider(&state, &user_id, "my-endpoint").await;
        let stranger = insert_user(state.pool(), "stranger@example.com").await;
        seed_custom_provider(&state, &stranger.id, "their-endpoint").await;

        let max_models: Vec<Value> = (0..MAX_MODELS_PER_GROUP)
            .map(|i| json!({ "name": format!("model-{i}"), "targets": ["claude-code/claude-opus-5"] }))
            .collect();
        let too_many_models: Vec<Value> = (0..=MAX_MODELS_PER_GROUP)
            .map(|i| json!({ "name": format!("model-{i}"), "targets": ["claude-code/claude-opus-5"] }))
            .collect();
        let too_many_targets: Vec<Value> = (0..=MAX_TARGETS_PER_MODEL).map(|i| json!(format!("claude-code/m{i}"))).collect();

        // Accepted: up to the caps, a '/' in a model name, and the caller's own custom slug.
        for (slug, models) in [
            ("max-ep", json!(max_models)),
            ("slash-ep", json!([{ "name": "claude-code/claude-opus-5", "targets": ["grok/grok-4.5"] }])),
            ("custom-ep", json!([{ "name": "opus", "targets": ["my-endpoint/gpt-4o"] }])),
        ] {
            let response = create_group(&state, &cookie, json!({ "name": slug, "slug": slug, "models": models })).await;
            assert_eq!(response.status(), StatusCode::CREATED, "{slug}");
        }

        for (label, models) in [
            ("empty models", json!([])),
            ("too many models", json!(too_many_models)),
            ("whitespace in a model name", json!([{ "name": "my model", "targets": ["grok/grok-4.5"] }])),
            (
                "duplicate model name",
                json!([{ "name": "opus", "targets": ["claude-code/claude-opus-5"] }, { "name": "opus", "targets": ["grok/grok-4.5"] }]),
            ),
            ("empty targets", json!([{ "name": "opus", "targets": [] }])),
            ("too many targets", json!([{ "name": "opus", "targets": too_many_targets }])),
            ("unknown provider prefix", json!([{ "name": "opus", "targets": ["not-a-real-provider/model"] }])),
            // A bare name would be a group targeting another group's model: no nesting, ever.
            ("a bare-name target", json!([{ "name": "opus", "targets": ["another-group-model"] }])),
            (
                "duplicate targets",
                json!([{ "name": "opus", "targets": ["claude-code/claude-opus-5", "claude-code/claude-opus-5"] }]),
            ),
            ("another user's custom slug", json!([{ "name": "opus", "targets": ["their-endpoint/gpt-4o"] }])),
        ] {
            let response = create_group(&state, &cookie, json!({ "name": label, "slug": "probe-ep", "models": models })).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{label}");
        }

        // The unknown-prefix message names the model it is about.
        let response =
            create_group(&state, &cookie, json!({ "name": "n", "slug": "probe-ep", "models": [{ "name": "opus", "targets": ["nope/model"] }] }))
                .await;
        assert!(body_json(response).await["error"].as_str().unwrap().contains("model \"opus\""));
    }

    #[tokio::test]
    async fn enforces_the_group_cap_per_user() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let (_, cookie) = signed_in(&state, "cap@example.com").await;
        for i in 0..MAX_MODEL_GROUPS_PER_USER {
            let response = create_group(&state, &cookie, json!({ "name": format!("group-{i}"), "slug": format!("slug-{i}") })).await;
            assert_eq!(response.status(), StatusCode::CREATED, "group {i}");
        }
        let response = create_group(&state, &cookie, json!({ "name": "one-too-many", "slug": "one-too-many" })).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn account_pinning_is_scoped_to_the_caller_and_the_targets_provider() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let (user_id, cookie) = signed_in(&state, "pin@example.com").await;
        let stranger = insert_user(state.pool(), "stranger@example.com").await;
        seed_account(&state, &user_id, "acc_1", "claude-code", Some("upstream@example.com"), Some("My Opus Account")).await;
        seed_account(&state, &user_id, "acc_2", "claude-code", Some("second@example.com"), None).await;
        seed_account(&state, &user_id, "acc_grok", "grok", None, None).await;
        seed_account(&state, &stranger.id, "acc_other", "claude-code", None, None).await;

        let models = |targets: Value| json!([{ "name": "opus", "targets": targets }]);
        let response = create_group(
            &state,
            &cookie,
            json!({ "name": "Pinned", "slug": "pinned-ep", "models": models(json!([{ "model": "claude-code/claude-opus-5", "account_id": "acc_1" }])) }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let json = body_json(response).await;
        assert_eq!(
            json["models"][0]["targets"],
            json!([{ "model": "claude-code/claude-opus-5", "account_id": "acc_1", "account_label": "My Opus Account" }]),
            "custom_label wins over the upstream label, read-time only"
        );

        // The same model pinned to two different accounts is two legitimate targets.
        let response = create_group(
            &state,
            &cookie,
            json!({ "name": "Two", "slug": "two-ep", "models": models(json!([
                { "model": "claude-code/claude-opus-5", "account_id": "acc_1" },
                { "model": "claude-code/claude-opus-5", "account_id": "acc_2" },
            ])) }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let json = body_json(response).await;
        assert_eq!(json["models"][0]["targets"].as_array().unwrap().len(), 2);
        assert_eq!(json["models"][0]["targets"][1]["account_label"], "second@example.com", "falls back to the upstream label");

        for (label, targets) in [
            ("another user's account", json!([{ "model": "claude-code/claude-opus-5", "account_id": "acc_other" }])),
            ("a provider mismatch", json!([{ "model": "claude-code/claude-opus-5", "account_id": "acc_grok" }])),
            ("a nonexistent account", json!([{ "model": "claude-code/claude-opus-5", "account_id": "acc_never_existed" }])),
            (
                "a duplicate (model, account) pair",
                json!([
                    { "model": "claude-code/claude-opus-5", "account_id": "acc_1" },
                    { "model": "claude-code/claude-opus-5", "account_id": "acc_1" },
                ]),
            ),
        ] {
            let response =
                create_group(&state, &cookie, json!({ "name": label, "slug": "probe-ep", "models": models(targets) })).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{label}");
        }

        // PUT validates a pinned account the same way.
        let created = body_json(create_group(&state, &cookie, json!({ "name": "PutBase", "slug": "put-ep" })).await).await;
        let id = created["id"].as_str().expect("the created group id");
        let response = send(
            &state,
            "PUT",
            &format!("/api/model-groups/{id}"),
            &cookie,
            Some(json!({ "models": models(json!([{ "model": "claude-code/claude-opus-5", "account_id": "acc_never_existed" }])) })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // A pinned account deleted later leaves the stale id with no label, never a 500.
        let pinned = body_json(
            create_group(
                &state,
                &cookie,
                json!({ "name": "Stale", "slug": "stale-ep", "models": models(json!([{ "model": "claude-code/claude-opus-5", "account_id": "acc_2" }])) }),
            )
            .await,
        )
        .await;
        sqlx::query("DELETE FROM upstream_accounts WHERE id = 'acc_2'").execute(state.pool()).await.unwrap();
        let json = body_json(send(&state, "GET", "/api/model-groups", &cookie, None).await).await;
        let group = json["groups"].as_array().unwrap().iter().find(|g| g["id"] == pinned["id"]).expect("the group");
        assert_eq!(
            group["models"][0]["targets"][0],
            json!({ "model": "claude-code/claude-opus-5", "account_id": "acc_2", "account_label": null })
        );
    }

    #[tokio::test]
    async fn update_patches_fields_and_replaces_the_whole_model_set() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let (_, cookie) = signed_in(&state, "update@example.com").await;
        let (_, other_cookie) = signed_in(&state, "updater2@example.com").await;
        let created = body_json(create_group(&state, &cookie, json!({})).await).await;
        let id = created["id"].as_str().unwrap().to_string();
        let uri = format!("/api/model-groups/{id}");

        // Another user's id and an unknown id are both 404, never an edit.
        assert_eq!(send(&state, "PUT", &uri, &other_cookie, Some(json!({ "name": "hijacked" }))).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            send(&state, "PUT", "/api/model-groups/mgrp_nonexistent", &cookie, Some(json!({ "name": "x" }))).await.status(),
            StatusCode::NOT_FOUND
        );

        // A rename touches neither slug nor models; an omitted strategy stays as stored.
        let json = body_json(send(&state, "PUT", &uri, &cookie, Some(json!({ "name": "Renamed Opus" }))).await).await;
        assert_eq!(json["name"], "Renamed Opus");
        assert_eq!(json["slug"], "opus-ep");
        assert_eq!(json["strategy"], "ordered");
        assert_eq!(json["models"].as_array().unwrap().len(), 1);
        assert_eq!(json["models"][0]["name"], "opus");

        // The slug is mutable — it moves the endpoint URL.
        let json = body_json(send(&state, "PUT", &uri, &cookie, Some(json!({ "slug": "moved-ep" }))).await).await;
        assert_eq!(json["slug"], "moved-ep");
        assert_eq!(json["name"], "Renamed Opus");
        // Keeping the group's own slug does not self-conflict.
        let response = send(&state, "PUT", &uri, &cookie, Some(json!({ "slug": "moved-ep", "name": "Still Opus" }))).await;
        assert_eq!(response.status(), StatusCode::OK);

        // Replacing models is a whole-set swap, never a per-entry patch.
        let json = body_json(
            send(&state, "PUT", &uri, &cookie, Some(json!({ "models": [{ "name": "gpt", "targets": ["codex/gpt-5.2"] }] }))).await,
        )
        .await;
        assert_eq!(json["models"].as_array().unwrap().len(), 1);
        assert_eq!(json["models"][0]["name"], "gpt");
        assert_eq!(
            json["models"][0]["targets"],
            json!([{ "model": "codex/gpt-5.2", "account_id": null, "account_label": null }])
        );
        assert_eq!(json["name"], "Still Opus");

        // A name or slug already used by another of the caller's groups is refused.
        create_group(&state, &cookie, json!({ "name": "Existing", "slug": "existing-ep" })).await;
        assert_eq!(send(&state, "PUT", &uri, &cookie, Some(json!({ "name": "Existing" }))).await.status(), StatusCode::BAD_REQUEST);
        assert_eq!(send(&state, "PUT", &uri, &cookie, Some(json!({ "slug": "existing-ep" }))).await.status(), StatusCode::BAD_REQUEST);

        // Invalid models are refused the same way as on create, leaving the stored set alone.
        let response =
            send(&state, "PUT", &uri, &cookie, Some(json!({ "models": [{ "name": "opus", "targets": ["bogus-provider/model"] }] }))).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body_json(send(&state, "GET", "/api/model-groups", &cookie, None).await).await;
        let group = json["groups"].as_array().unwrap().iter().find(|g| g["id"] == id.as_str()).expect("the group");
        assert_eq!(group["models"][0]["targets"], json!([{ "model": "codex/gpt-5.2", "account_id": null, "account_label": null }]));
    }

    #[tokio::test]
    async fn strategy_accepts_only_ordered() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let (_, cookie) = signed_in(&state, "strategy@example.com").await;

        let created = body_json(create_group(&state, &cookie, json!({ "strategy": "ordered" })).await).await;
        assert_eq!(created["strategy"], "ordered");
        let id = created["id"].as_str().unwrap().to_string();
        let uri = format!("/api/model-groups/{id}");

        let response = create_group(&state, &cookie, json!({ "name": "Other", "slug": "other-ep", "strategy": "usage-balanced" })).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(response).await["error"].as_str().unwrap().to_lowercase().contains("strategy"));

        let json = body_json(send(&state, "PUT", &uri, &cookie, Some(json!({ "strategy": "ordered" }))).await).await;
        assert_eq!(json["strategy"], "ordered");

        // An unknown value is refused and leaves the stored value untouched.
        let response = send(&state, "PUT", &uri, &cookie, Some(json!({ "strategy": "spend-aware" }))).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body_json(send(&state, "GET", "/api/model-groups", &cookie, None).await).await;
        assert_eq!(json["groups"][0]["strategy"], "ordered");
    }

    #[tokio::test]
    async fn delete_is_scoped_to_the_owner_and_takes_the_model_rows() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let (_, cookie) = signed_in(&state, "delete@example.com").await;
        let (_, other_cookie) = signed_in(&state, "deleter2@example.com").await;
        let created = body_json(create_group(&state, &cookie, json!({})).await).await;
        let uri = format!("/api/model-groups/{}", created["id"].as_str().unwrap());

        assert_eq!(send(&state, "DELETE", &uri, &other_cookie, None).await.status(), StatusCode::NOT_FOUND);
        let json = body_json(send(&state, "GET", "/api/model-groups", &cookie, None).await).await;
        assert_eq!(json["groups"].as_array().unwrap().len(), 1, "still present for the owner");

        let response = send(&state, "DELETE", &uri, &cookie, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await, json!({ "ok": true }));
        let json = body_json(send(&state, "GET", "/api/model-groups", &cookie, None).await).await;
        assert_eq!(json["groups"].as_array().unwrap().len(), 0);
        let models: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_group_models").fetch_one(state.pool()).await.unwrap();
        assert_eq!(models, 0);
    }

    #[tokio::test]
    async fn a_target_may_name_a_shared_endpoint_slug_but_never_pin_its_account() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let owner = insert_user(&pool, "owner@example.com").await;
        let lent = owner_endpoint(&pool, &owner.id, "team-gw", "openai", "https://owner.example.com/v1", None).await;
        let stranger = insert_user(&pool, "stranger@example.com").await;
        owner_endpoint(&pool, &stranger.id, "secret-gw", "openai", "https://stranger.example.com/v1", None).await;
        let ext = SharedCustomExtension::lending(vec![lent.clone()]);
        let state = state_with_extension(pool, MockTransport::new(), ext.clone());
        let (_, cookie) = signed_in(&state, "borrower@example.com").await;
        let models = |targets: Value| json!([{ "name": "gpt", "targets": targets }]);

        let response =
            create_group(&state, &cookie, json!({ "name": "Shared", "slug": "shared-ep", "models": models(json!(["team-gw/gpt-4o"])) })).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let json = body_json(response).await;
        // The routing indicator walks the borrowed key like any other candidate.
        assert_eq!(json["models"][0]["routing"]["current_target_index"], 0);

        for (label, targets) in [
            ("a stranger's unshared slug", json!(["secret-gw/gpt-4o"])),
            ("a pin on the shared endpoint's account", json!([{ "model": "team-gw/gpt-4o", "account_id": lent.account.id }])),
        ] {
            let response = create_group(&state, &cookie, json!({ "name": label, "slug": "probe-ep", "models": models(targets) })).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{label}");
        }

        // Two groups with three targets on the shared slug: the whole list asks once.
        let response = create_group(
            &state,
            &cookie,
            json!({ "name": "More", "slug": "more-ep", "models": models(json!(["team-gw/gpt-4o-mini", "team-gw/o3"])) }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let before = ext.list_calls.lock().unwrap().len();
        let json = body_json(send(&state, "GET", "/api/model-groups", &cookie, None).await).await;
        assert_eq!(json["groups"].as_array().unwrap().len(), 2);
        assert_eq!(ext.list_calls.lock().unwrap().len(), before + 1);
    }

    #[tokio::test]
    async fn a_target_may_pin_a_builtin_account_while_it_is_shared_with_the_caller() {
        use crate::pool::extension::test_support::share_info;
        use crate::pool::SharedAccount;

        let Some(pool) = test_pool().await else { return skip_without_db() };
        let ext = std::sync::Arc::new(SharedCustomExtension::default());
        let state = state_with_extension(pool, MockTransport::new(), ext.clone());
        let (user_id, cookie) = signed_in(&state, "borrower@example.com").await;
        let owner = insert_user(state.pool(), "owner@example.com").await;
        let stranger = insert_user(state.pool(), "stranger@example.com").await;
        seed_account(&state, &user_id, "acc_mine", "claude-code", Some("me@example.com"), None).await;
        seed_account(&state, &owner.id, "acc_lent", "claude-code", Some("owner@example.com"), Some("Owner Max")).await;
        seed_account(&state, &stranger.id, "acc_unshared", "claude-code", Some("stranger@example.com"), None).await;
        let lent = get_account(state.pool(), &owner.id, "acc_lent").await.unwrap().unwrap();
        ext.accounts.lock().unwrap().push(SharedAccount { account: lent, priority: 0, share: share_info(), usage: None });
        let pin = |model: &str, id: &str| json!({ "model": model, "account_id": id });
        let models = |targets: Value| json!([{ "name": "opus", "targets": targets }]);

        let ordered = json!([pin("claude-code/claude-opus-5", "acc_lent"), pin("claude-code/claude-opus-5", "acc_mine")]);
        let response = create_group(&state, &cookie, json!({ "name": "Team", "slug": "team-ep", "models": models(ordered) })).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let json = body_json(response).await;
        assert_eq!(json["models"][0]["targets"][0]["account_label"], "Owner Max");
        assert_eq!(json["models"][0]["routing"]["current_target_index"], 0, "the shared pin is the route");

        for (label, targets) in [
            ("an account nobody shares with the caller", json!([pin("claude-code/claude-opus-5", "acc_unshared")])),
            ("a shared account under another provider's prefix", json!([pin("codex/gpt-5", "acc_lent")])),
        ] {
            let response = create_group(&state, &cookie, json!({ "name": label, "slug": "probe-ep", "models": models(targets) })).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{label}");
        }

        // A second group with the same shared pin: listing both asks the extension once.
        let again = json!([pin("claude-code/claude-opus-5", "acc_lent")]);
        let response = create_group(&state, &cookie, json!({ "name": "Again", "slug": "again-ep", "models": models(again) })).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let before = ext.account_calls.lock().unwrap().len();
        send(&state, "GET", "/api/model-groups", &cookie, None).await;
        assert_eq!(ext.account_calls.lock().unwrap().len(), before + 1);

        // Unshared: the pin stays stored but reads as gone, and the route falls to the own pin.
        ext.accounts.lock().unwrap().clear();
        let json = body_json(send(&state, "GET", "/api/model-groups", &cookie, None).await).await;
        let team = json["groups"].as_array().unwrap().iter().find(|g| g["slug"] == "team-ep").expect("the group");
        assert_eq!(team["models"][0]["targets"][0]["account_id"], "acc_lent");
        assert_eq!(team["models"][0]["targets"][0]["account_label"], Value::Null);
        assert_eq!(team["models"][0]["routing"]["targets"][0]["reason"], "no_account");
        assert_eq!(team["models"][0]["routing"]["current_target_index"], 1);
    }

    #[tokio::test]
    async fn an_update_keeps_stored_targets_that_no_longer_resolve_but_checks_new_ones() {
        use crate::pool::extension::test_support::share_info;
        use crate::pool::SharedAccount;

        let Some(pool) = test_pool().await else { return skip_without_db() };
        let owner = insert_user(&pool, "owner@example.com").await;
        let endpoint = owner_endpoint(&pool, &owner.id, "team-gw", "openai", "https://owner.example.com/v1", None).await;
        let ext = SharedCustomExtension::lending(vec![endpoint]);
        let state = state_with_extension(pool, MockTransport::new(), ext.clone());
        let (user_id, cookie) = signed_in(&state, "borrower@example.com").await;
        seed_account(&state, &user_id, "acc_mine", "claude-code", Some("me@example.com"), None).await;
        seed_account(&state, &owner.id, "acc_lent", "claude-code", Some("owner@example.com"), None).await;
        let lent = get_account(state.pool(), &owner.id, "acc_lent").await.unwrap().unwrap();
        ext.accounts.lock().unwrap().push(SharedAccount { account: lent, priority: 0, share: share_info(), usage: None });
        let lent_pin = json!({ "model": "claude-code/claude-opus-5", "account_id": "acc_lent" });
        let targets = json!([lent_pin.clone(), "team-gw/gpt-4o", "claude-code/claude-opus-5"]);
        let response = create_group(
            &state,
            &cookie,
            json!({ "name": "Team", "slug": "team-ep", "models": [{ "name": "opus", "targets": targets }] }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let id = body_json(response).await["id"].as_str().unwrap().to_string();

        // Both shares are turned off: neither the pin nor the shared slug resolves any more.
        ext.accounts.lock().unwrap().clear();
        ext.shared.lock().unwrap().clear();
        let path = format!("/api/model-groups/{id}");

        // Reordering the group and adding a model of the caller's own keeps both as stored.
        let reordered = json!([
            { "name": "opus", "targets": ["claude-code/claude-opus-5", "team-gw/gpt-4o", lent_pin.clone()] },
            { "name": "sonnet", "targets": [{ "model": "claude-code/claude-sonnet-5", "account_id": "acc_mine" }] },
        ]);
        let response = send(&state, "PUT", &path, &cookie, Some(json!({ "models": reordered }))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["models"][0]["targets"][2]["account_id"], "acc_lent");
        assert_eq!(json["models"][0]["targets"][2]["account_label"], Value::Null);
        assert_eq!(json["models"][0]["targets"][1]["model"], "team-gw/gpt-4o");

        // A target the group never stored is checked as on create, even on the same account
        // or the same slug.
        for (label, target) in [
            ("a new model pinned to the unresolved account", json!({ "model": "claude-code/claude-sonnet-5", "account_id": "acc_lent" })),
            ("a new model on the unresolved slug", json!("team-gw/o3")),
        ] {
            let models = json!([{ "name": "opus", "targets": [lent_pin.clone(), target] }]);
            let response = send(&state, "PUT", &path, &cookie, Some(json!({ "models": models }))).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{label}");
        }

        // Another group cannot borrow this group's stored pin.
        let response = create_group(
            &state,
            &cookie,
            json!({ "name": "Copy", "slug": "copy-ep", "models": [{ "name": "opus", "targets": [lent_pin] }] }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_failing_share_store_fails_a_shared_pin_write_but_never_an_own_one() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let ext = std::sync::Arc::new(SharedCustomExtension { fail_accounts: true, ..Default::default() });
        let state = state_with_extension(pool, MockTransport::new(), ext.clone());
        let (user_id, cookie) = signed_in(&state, "borrower@example.com").await;
        seed_account(&state, &user_id, "acc_mine", "claude-code", None, None).await;
        let models = |id: &str| json!([{ "name": "opus", "targets": [{ "model": "claude-code/claude-opus-5", "account_id": id }] }]);

        let response = create_group(&state, &cookie, json!({ "name": "Own", "slug": "own-ep", "models": models("acc_mine") })).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(ext.account_calls.lock().unwrap().is_empty(), "an own pin never asks the extension");

        let response = create_group(&state, &cookie, json!({ "name": "Other", "slug": "other-ep", "models": models("acc_elsewhere") })).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR, "a lost store is retryable, not a 400");
    }

    #[tokio::test]
    async fn a_failing_extension_never_blocks_a_group_of_the_callers_own_providers() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let ext = std::sync::Arc::new(SharedCustomExtension { fail: true, ..Default::default() });
        let state = state_with_extension(pool, MockTransport::new(), ext.clone());
        let (_, cookie) = signed_in(&state, "borrower@example.com").await;
        let body = json!({ "name": "Own", "slug": "own-ep", "models": [{ "name": "sonnet", "targets": ["claude-code/claude-sonnet-4"] }] });
        let response = create_group(&state, &cookie, body).await;
        assert!(response.status().is_success(), "{}", response.status());
        assert!(ext.list_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failing_extension_fails_the_write_instead_of_rejecting_a_shared_slug() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let ext = std::sync::Arc::new(SharedCustomExtension { fail: true, ..Default::default() });
        let state = state_with_extension(pool, MockTransport::new(), ext);
        let (_, cookie) = signed_in(&state, "borrower@example.com").await;
        let body = json!({ "name": "Shared", "slug": "shared-ep", "models": [{ "name": "gpt", "targets": ["team-gw/gpt-4o"] }] });
        let response = create_group(&state, &cookie, body).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
