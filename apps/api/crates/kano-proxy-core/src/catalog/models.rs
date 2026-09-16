//! The model catalog for one user's bound providers (docs/api.md § Models, docs/cli.md § Model catalog).
//!
//! The model list for one user's bound providers. Builtin pools are queried live through their
//! adapter (codex has no models endpoint, so it contributes nothing — no invented catalog);
//! custom providers are manual or live per `models_mode`; CLI providers report their own list
//! through the tunnel and are read straight from storage. Nothing here fabricates a model.
//!
//! The one-hour `models:v1:<user>:<provider>` cache is kept (unlike the per-account usage
//! cache): the client-facing `/openai/v1/models` and `/anthropic/v1/models` are called by API
//! clients with no frontend cache. `force` bypasses it.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::app::now_ms;
use crate::db::accounts::{list_accounts, AccountRow};
use crate::db::cli::{exposed_cli_models, list_cli_providers};
use crate::db::custom_providers::{list_custom_providers, CustomProviderRow};
use crate::pool::acquire::{acquire, AcquiredAccount};
use crate::pool::bench::bench_until_from_row;
use crate::pool::extension::ListSharedOptions;
use crate::providers::registry::get_adapter;
use crate::providers::types::UpstreamModel;
use crate::providers::{ProviderId, PROVIDERS};
use crate::routing::candidates::custom_provider_adapter;
use crate::utils::custom_provider::parse_manual_models;
use crate::AppState;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    /// Builtin `ProviderId`, or a custom/CLI provider's slug.
    pub provider: String,
    pub upstream: String,
    pub display_name: String,
    pub available: bool,
    pub owned_by: String,
    /// Always `"model"`.
    pub object: String,
}

impl CatalogModel {
    fn new(provider: &str, model: &UpstreamModel) -> Self {
        Self {
            id: format!("{provider}/{}", model.id),
            provider: provider.to_string(),
            upstream: model.id.clone(),
            display_name: model
                .display_name
                .clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| model.id.clone()),
            available: true,
            owned_by: provider.to_string(),
            object: "model".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderModelsSection {
    pub provider: String,
    pub models: Vec<CatalogModel>,
    pub error: Option<String>,
    pub cached: bool,
}

const MODELS_CACHE_TTL: Duration = Duration::from_secs(3600);

/// `provider` is a builtin `ProviderId` or a custom provider's slug.
fn models_cache_key(user_id: &str, provider: &str) -> String {
    format!("models:v1:{user_id}:{provider}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedModels {
    models: Vec<CatalogModel>,
    error: Option<String>,
    #[serde(rename = "fetchedAt")]
    fetched_at: i64,
}

async fn read_models_cache(cx: &AppState, user_id: &str, provider: &str) -> Option<CachedModels> {
    let snapshot: CachedModels = cx.cache().get_json(&models_cache_key(user_id, provider)).await?;
    // The in-process cache expires on its own; the stamp check keeps the TypeScript guard for a
    // snapshot that outlived its TTL by any other route.
    if now_ms() - snapshot.fetched_at > MODELS_CACHE_TTL.as_millis() as i64 + 5_000 {
        return None;
    }
    Some(snapshot)
}

async fn write_models_cache(cx: &AppState, user_id: &str, provider: &str, models: Vec<CatalogModel>, error: Option<String>) {
    let snapshot = CachedModels { models, error, fetched_at: now_ms() };
    cx.cache().put_json(&models_cache_key(user_id, provider), &snapshot, MODELS_CACHE_TTL).await;
}

/// A bound, non-benched, decryptable account to query the live list with. `shared` are the rows
/// a pool extension offers this viewer for a builtin provider (docs/cloud-edition.md § "Pool
/// extension") — tried only after the viewer's own rows, and their credential is decrypted here
/// exactly like an own one and never returned to the client.
async fn pick_usable_account(
    cx: &AppState,
    user_id: &str,
    provider: &str,
    shared: Vec<AccountRow>,
) -> Option<AcquiredAccount> {
    let own = list_accounts(cx.pool(), user_id, provider).await.unwrap_or_default();
    let now = now_ms();
    for row in own.into_iter().chain(shared) {
        if bench_until_from_row(&row, now).is_some() {
            continue;
        }
        if let Ok(acquired) = acquire(cx, row) {
            return Some(acquired);
        }
    }
    None
}

async fn fetch_provider_models(
    cx: &AppState,
    user_id: &str,
    provider: ProviderId,
    force: bool,
    shared: Vec<AccountRow>,
) -> ProviderModelsSection {
    let slug = provider.as_str();
    if !force {
        if let Some(cached) = read_models_cache(cx, user_id, slug).await {
            return ProviderModelsSection {
                provider: slug.to_string(),
                models: cached.models,
                error: cached.error,
                cached: true,
            };
        }
    }

    let Some(account) = pick_usable_account(cx, user_id, slug, shared).await else {
        // Don't cache "no account" aggressively — skip the write entirely.
        return ProviderModelsSection { provider: slug.to_string(), models: Vec::new(), error: None, cached: false };
    };

    let adapter = get_adapter(provider);
    if !adapter.has_list_models() {
        return ProviderModelsSection {
            provider: slug.to_string(),
            models: Vec::new(),
            error: Some("provider has no listModels".into()),
            cached: false,
        };
    }

    // A refresh failure is not a catalog failure: the stored credential still gets a turn, and
    // the adapter reports whatever the upstream says about it.
    let account = match adapter.refresh_if_needed(cx, account.clone()).await {
        Ok(refreshed) => refreshed,
        Err(error) => {
            tracing::debug!(%error, provider = slug, "catalog credential refresh failed");
            account
        }
    };

    let result = adapter.list_models(cx, &account).await;
    let models: Vec<CatalogModel> = result.models.iter().map(|m| CatalogModel::new(slug, m)).collect();
    write_models_cache(cx, user_id, slug, models.clone(), result.error.clone()).await;
    ProviderModelsSection { provider: slug.to_string(), models, error: result.error, cached: false }
}

/// manual → the stored manual list. auto → live `list_models` with an acquired key (same 1h
/// cache as built-ins, keyed by slug); on failure, or when no usable key exists to query, fall
/// back to the manual list if non-empty, else empty. Never fabricates a catalog.
async fn fetch_custom_provider_models(
    cx: &AppState,
    user_id: &str,
    row: &CustomProviderRow,
    force: bool,
) -> ProviderModelsSection {
    let manual_models = || -> Vec<CatalogModel> {
        parse_manual_models(row.manual_models_json.as_deref())
            .into_iter()
            .map(|id| CatalogModel::new(&row.slug, &UpstreamModel { id, display_name: None }))
            .collect()
    };

    if row.models_mode == "manual" {
        return ProviderModelsSection {
            provider: row.slug.clone(),
            models: manual_models(),
            error: None,
            cached: false,
        };
    }

    if !force {
        if let Some(cached) = read_models_cache(cx, user_id, &row.slug).await {
            return ProviderModelsSection {
                provider: row.slug.clone(),
                models: cached.models,
                error: cached.error,
                cached: true,
            };
        }
    }

    let Some(account) = pick_usable_account(cx, user_id, &row.slug, Vec::new()).await else {
        // No usable key to query live — same "don't cache aggressively" policy as built-ins.
        return ProviderModelsSection {
            provider: row.slug.clone(),
            models: manual_models(),
            error: None,
            cached: false,
        };
    };

    let adapter = custom_provider_adapter(row);
    if !adapter.has_list_models() {
        return ProviderModelsSection {
            provider: row.slug.clone(),
            models: manual_models(),
            error: None,
            cached: false,
        };
    }

    let result = adapter.list_models(cx, &account).await;
    if let Some(error) = result.error {
        let fallback = manual_models();
        write_models_cache(cx, user_id, &row.slug, fallback.clone(), Some(error.clone())).await;
        return ProviderModelsSection {
            provider: row.slug.clone(),
            models: fallback,
            error: Some(error),
            cached: false,
        };
    }

    let models: Vec<CatalogModel> = result.models.iter().map(|m| CatalogModel::new(&row.slug, m)).collect();
    write_models_cache(cx, user_id, &row.slug, models.clone(), None).await;
    ProviderModelsSection { provider: row.slug.clone(), models, error: None, cached: false }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ListModelsOptions {
    /// Kept for API shape; every returned model is available, so this filters nothing.
    pub available_only: bool,
    /// Bypass the one-hour cache (`?refresh=true`).
    pub force: bool,
}

pub struct UserModels {
    pub models: Vec<CatalogModel>,
    pub providers: Vec<ProviderModelsSection>,
}

/// Live models for a user. Only providers with bound accounts are queried.
pub async fn list_models_for_user(cx: &AppState, user_id: &str, opts: ListModelsOptions) -> UserModels {
    let mut sections: Vec<ProviderModelsSection> = Vec::new();

    for provider in PROVIDERS {
        // A builtin provider counts as bound when the viewer has rows of their own **or** rows
        // shared with them (docs/cloud-edition.md § "Pool extension"); the shared credential
        // only ever leaves this process as an upstream Authorization header, never in the
        // response.
        let shared: Vec<AccountRow> = match cx.pool_extension() {
            Some(ext) => match ext.list_shared(cx, user_id, provider, ListSharedOptions::default()).await {
                Ok(rows) => rows.into_iter().map(|s| s.account).collect(),
                Err(error) => {
                    tracing::error!(%error, "pool extension listShared failed for the model catalog");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        sections.push(fetch_provider_models(cx, user_id, provider, opts.force, shared).await);
    }

    for row in list_custom_providers(cx.pool(), user_id).await.unwrap_or_default() {
        sections.push(fetch_custom_provider_models(cx, user_id, &row, opts.force).await);
    }

    // CLI providers (docs/cli.md § Model catalog): one section per provider, straight from the
    // stored agent report with the expose filter applied at read time — no fetch, no cache, no
    // fabrication. An offline agent keeps showing its last real report; one that never connected
    // shows empty.
    for row in list_cli_providers(cx.pool(), user_id).await.unwrap_or_default() {
        let models = exposed_cli_models(row.models_json.as_deref(), row.model_filter_json.as_deref())
            .into_iter()
            .map(|id| CatalogModel::new(&row.slug, &UpstreamModel { id, display_name: None }))
            .collect();
        sections.push(ProviderModelsSection { provider: row.slug, models, error: None, cached: false });
    }

    // Model groups are deliberately absent here (since v4): each group is its own endpoint whose
    // /models lists its names (docs/api.md § Group endpoints).
    let models = sections.iter().flat_map(|s| s.models.iter().cloned()).collect();
    UserModels { models, providers: sections }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{skip_without_db, test_pool, test_state};
    use crate::pool::StoredCredential;
    use crate::upstream::{MockTransport, UpstreamResponse};
    use http::StatusCode;
    use serde_json::json;
    use sqlx::PgPool;

    async fn seed_user(pool: &PgPool) -> String {
        let user = crate::db::test_support::insert_user(pool, &format!("cat-{}@example.com", crate::ids::new_id(""))).await;
        user.id
    }

    async fn seed_custom_provider(
        pool: &PgPool,
        user_id: &str,
        slug: &str,
        format: &str,
        models_mode: &str,
        manual: Option<&str>,
    ) {
        let base = if format == "openai" { "https://upstream.example.com/v1" } else { "https://upstream.example.com" };
        sqlx::query(
            "INSERT INTO custom_providers (id, user_id, slug, name, format, base_url, models_mode,
                                           manual_models_json, sort_order, created_at, updated_at)
             VALUES ($1, $2, $3, $3, $4, $5, $6, $7, 0, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(format!("cprov_{slug}"))
        .bind(user_id)
        .bind(slug)
        .bind(format)
        .bind(base)
        .bind(models_mode)
        .bind(manual)
        .execute(pool)
        .await
        .expect("seed custom provider");
    }

    async fn seed_key(pool: &PgPool, user_id: &str, provider: &str) {
        crate::db::test_support::insert_account(
            pool,
            user_id,
            provider,
            &StoredCredential { access_token: "sk-test".into(), ..Default::default() },
        )
        .await;
    }

    fn section<'a>(sections: &'a [ProviderModelsSection], slug: &str) -> Option<&'a ProviderModelsSection> {
        sections.iter().find(|s| s.provider == slug)
    }

    fn ids(models: &[CatalogModel], slug: &str) -> Vec<String> {
        models.iter().filter(|m| m.provider == slug).map(|m| m.id.clone()).collect()
    }

    #[tokio::test]
    async fn manual_mode_returns_the_stored_list_without_ever_fetching() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        // A transport with no queued handler panics on any call, so "never fetches" is enforced.
        let state = test_state(pool, MockTransport::new());
        let user = seed_user(state.pool()).await;
        seed_custom_provider(
            state.pool(),
            &user,
            "manual-ep",
            "openai",
            "manual",
            Some(&json!(["model-a", "model-b"]).to_string()),
        )
        .await;
        seed_key(state.pool(), &user, "manual-ep").await;

        let out = list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        assert_eq!(ids(&out.models, "manual-ep"), ["manual-ep/model-a", "manual-ep/model-b"]);
        assert_eq!(section(&out.providers, "manual-ep").unwrap().error, None);
    }

    #[tokio::test]
    async fn auto_mode_fetches_live_models_and_prefixes_ids_with_the_slug() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "data": [{ "id": "live-model" }] }));
        let state = test_state(pool, transport.clone());
        let user = seed_user(state.pool()).await;
        seed_custom_provider(state.pool(), &user, "auto-ep", "openai", "auto", None).await;
        seed_key(state.pool(), &user, "auto-ep").await;

        let out = list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        assert_eq!(ids(&out.models, "auto-ep"), ["auto-ep/live-model"]);
        assert_eq!(transport.requests().len(), 1);
    }

    #[tokio::test]
    async fn auto_mode_caches_the_live_result_for_the_one_hour_window() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "data": [{ "id": "m1" }] }));
        let state = test_state(pool, transport.clone());
        let user = seed_user(state.pool()).await;
        seed_custom_provider(state.pool(), &user, "cached-ep", "openai", "auto", None).await;
        seed_key(state.pool(), &user, "cached-ep").await;

        list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        let second = list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        assert_eq!(transport.requests().len(), 1, "the second call is served from the cache");
        assert!(section(&second.providers, "cached-ep").unwrap().cached);
        assert_eq!(ids(&second.models, "cached-ep"), ["cached-ep/m1"]);
    }

    #[tokio::test]
    async fn force_bypasses_the_cache() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "data": [{ "id": "m1" }] }));
        transport.respond_json(StatusCode::OK, json!({ "data": [{ "id": "m2" }] }));
        let state = test_state(pool, transport.clone());
        let user = seed_user(state.pool()).await;
        seed_custom_provider(state.pool(), &user, "force-ep", "openai", "auto", None).await;
        seed_key(state.pool(), &user, "force-ep").await;

        list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        let refreshed =
            list_models_for_user(&state, &user, ListModelsOptions { force: true, ..Default::default() }).await;
        assert_eq!(transport.requests().len(), 2);
        assert_eq!(ids(&refreshed.models, "force-ep"), ["force-ep/m2"]);
    }

    #[tokio::test]
    async fn an_auto_mode_failure_falls_back_to_a_non_empty_manual_list() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.expect(|_| {
            Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, Default::default(), "server error"))
        });
        let state = test_state(pool, transport.clone());
        let user = seed_user(state.pool()).await;
        seed_custom_provider(
            state.pool(),
            &user,
            "fallback-ep",
            "openai",
            "auto",
            Some(&json!(["manual-fallback"]).to_string()),
        )
        .await;
        seed_key(state.pool(), &user, "fallback-ep").await;

        let out = list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        assert_eq!(ids(&out.models, "fallback-ep"), ["fallback-ep/manual-fallback"]);
        assert_eq!(section(&out.providers, "fallback-ep").unwrap().error.as_deref(), Some("models 500"));
    }

    #[tokio::test]
    async fn an_auto_mode_failure_with_no_manual_list_returns_empty_never_fabricated() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.expect(|_| {
            Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, Default::default(), "server error"))
        });
        let state = test_state(pool, transport);
        let user = seed_user(state.pool()).await;
        seed_custom_provider(state.pool(), &user, "empty-ep", "openai", "auto", None).await;
        seed_key(state.pool(), &user, "empty-ep").await;

        let out = list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        assert!(ids(&out.models, "empty-ep").is_empty());
    }

    #[tokio::test]
    async fn auto_mode_with_no_usable_account_falls_back_to_the_manual_list_without_fetching() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = seed_user(state.pool()).await;
        seed_custom_provider(
            state.pool(),
            &user,
            "no-key-ep",
            "anthropic",
            "auto",
            Some(&json!(["manual-only"]).to_string()),
        )
        .await;
        // No account row at all.

        let out = list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        assert_eq!(ids(&out.models, "no-key-ep"), ["no-key-ep/manual-only"]);
    }

    #[tokio::test]
    async fn a_benched_or_undecryptable_account_is_never_used_to_query() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = seed_user(state.pool()).await;
        seed_custom_provider(state.pool(), &user, "benched-ep", "openai", "auto", None).await;
        seed_key(state.pool(), &user, "benched-ep").await;
        sqlx::query("UPDATE upstream_accounts SET bench_until = $1 WHERE user_id = $2")
            .bind(crate::db::accounts::iso_from_ms(now_ms() + 600_000))
            .bind(&user)
            .execute(state.pool())
            .await
            .unwrap();

        // The transport has no handler, so any live call would panic.
        let out = list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        assert!(ids(&out.models, "benched-ep").is_empty());
    }

    #[tokio::test]
    async fn an_anthropic_format_provider_queries_base_v1_models() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "data": [{ "id": "claude-3" }] }));
        let state = test_state(pool, transport.clone());
        let user = seed_user(state.pool()).await;
        seed_custom_provider(state.pool(), &user, "claude-like", "anthropic", "auto", None).await;
        seed_key(state.pool(), &user, "claude-like").await;

        list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        assert_eq!(transport.requests()[0].url, "https://upstream.example.com/v1/models");
    }

    #[tokio::test]
    async fn cli_providers_report_their_own_list_with_the_expose_filter_applied() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = seed_user(state.pool()).await;
        sqlx::query(
            "INSERT INTO cli_providers (id, user_id, slug, name, format, models_json, model_filter_json,
                                        sort_order, created_at, updated_at)
             VALUES ('cli_1', $1, 'laptop', 'laptop', 'openai', $2, $3, 0,
                     '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(&user)
        .bind(json!(["a", "b", "c"]).to_string())
        .bind(json!(["a", "c"]).to_string())
        .execute(state.pool())
        .await
        .unwrap();

        let out = list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        assert_eq!(ids(&out.models, "laptop"), ["laptop/a", "laptop/c"]);
        let section = section(&out.providers, "laptop").unwrap();
        assert_eq!(section.error, None);
        assert!(!section.cached);
    }

    #[tokio::test]
    async fn model_groups_are_absent_from_the_shared_catalog() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = seed_user(state.pool()).await;
        sqlx::query(
            "INSERT INTO model_groups (id, user_id, name, slug, strategy, created_at, updated_at)
             VALUES ('mgrp_1', $1, 'opus', 'team', 'ordered', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(&user)
        .execute(state.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO model_group_models (id, user_id, group_id, name, targets_json, created_at, updated_at)
             VALUES ('mgmodel_1', $1, 'mgrp_1', 'opus', $2, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(&user)
        .bind(json!(["claude-code/claude-opus-5"]).to_string())
        .execute(state.pool())
        .await
        .unwrap();

        let out = list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        assert!(out.models.iter().all(|m| m.id != "opus"));
        assert!(out.models.iter().all(|m| m.provider != "group"));
        assert!(section(&out.providers, "group").is_none());
        // A group is never expanded into its targets either.
        assert!(out.models.iter().all(|m| m.id != "claude-code/claude-opus-5"));
    }

    #[tokio::test]
    async fn every_builtin_pool_gets_a_section_even_with_nothing_bound() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = seed_user(state.pool()).await;
        let out = list_models_for_user(&state, &user, ListModelsOptions::default()).await;
        let mut provider_ids: Vec<String> = out.providers.iter().map(|s| s.provider.clone()).collect();
        provider_ids.sort();
        assert_eq!(provider_ids, ["antigravity", "claude-code", "codex", "grok"]);
        assert!(out.models.is_empty(), "nothing bound means nothing listed, never an invented catalog");
    }

    #[test]
    fn client_model_ids_use_the_provider_slash_upstream_shape() {
        let model = CatalogModel::new("claude-code", &UpstreamModel { id: "claude-opus-4-6".into(), display_name: None });
        assert_eq!(model.id, "claude-code/claude-opus-4-6");
        assert_eq!(model.display_name, "claude-opus-4-6");
        assert_eq!(model.owned_by, "claude-code");
        assert_eq!(model.object, "model");
        assert!(model.available);
        let named = CatalogModel::new("grok", &UpstreamModel { id: "grok-4.5".into(), display_name: Some("Grok".into()) });
        assert_eq!(named.display_name, "Grok");
    }

    #[test]
    fn the_cache_key_keeps_its_kv_shape() {
        assert_eq!(models_cache_key("user_1", "grok"), "models:v1:user_1:grok");
    }
}
