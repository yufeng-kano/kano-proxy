//! Candidate selection (docs/providers.md § Routing module
//! "Candidates").
//!
//! Turns a resolved request — a direct `provider/model`, or a group endpoint's model — into
//! one flat, ordered [`RoutingCandidate`] list:
//!
//! - A **direct** call is one implicit unpinned target: that pool's accounts in priority order.
//! - A **group model** expands each target of its list in array order: a pinned target
//!   contributes exactly its one account (if it still exists); an unpinned target contributes
//!   every account of that provider's pool, in pool-priority order.
//!
//! A target whose prefix no longer resolves (e.g. a deleted custom provider) contributes
//! nothing. So does model eligibility: an account whose adapter says it cannot serve the
//! requested model (`supports_model`, read from stored profile facts) contributes nothing for
//! that model. Usability (bench / usage-window exhaustion) is NOT decided here — see
//! [`crate::routing::facts`]; this module only decides which `(provider, upstream_model,
//! account)` triples exist at all.

use crate::db::accounts::{account_profile_meta, get_account, list_accounts, AccountRow};
use crate::db::cli::{get_cli_provider_by_slug, CliProviderRow};
use crate::db::custom_providers::{get_custom_provider_by_slug, CustomProviderRow};
use crate::db::model_groups::{
    get_group_model_by_name, parse_group_targets, GroupTarget, ModelGroupModelRow, ModelGroupRow,
};
use crate::db::provider_settings::get_provider_strategy;
use crate::pool::extension::{ListSharedOptions, ShareInfo};
use crate::providers::cli::create_cli_adapter;
use crate::providers::custom_anthropic::custom_anthropic_adapter;
use crate::providers::custom_openai::custom_openai_adapter;
use crate::providers::registry::get_adapter;
use crate::providers::{DynAdapter, ProviderId};
use crate::routing::types::RoutingCandidate;
use crate::utils::model::split_model_id;
use crate::AppState;

/// A target/direct-call's provider once its `model` prefix has resolved — before accounts are
/// looked up.
#[derive(Clone)]
pub struct ResolvedTarget {
    pub target_index: usize,
    pub provider: String,
    pub upstream_model: String,
    pub is_builtin: bool,
    pub custom_provider: Option<CustomProviderRow>,
    pub adapter: DynAdapter,
    /// Pinned `upstream_accounts` id, or `None` for an unpinned (whole-pool) target.
    pub account_id: Option<String>,
}

/// The adapter for one custom (BYO base URL) provider row — `adapterFor(false, …)`.
pub fn custom_provider_adapter(row: &CustomProviderRow) -> DynAdapter {
    if row.format == "anthropic" {
        custom_anthropic_adapter(row.clone())
    } else {
        custom_openai_adapter(row.clone())
    }
}

/// The adapter for one CLI provider row (docs/cli.md).
pub fn cli_provider_adapter(cx: &AppState, row: &CliProviderRow) -> DynAdapter {
    create_cli_adapter(cx, row)
}

/// Split one group target's `model` string into a resolved provider/adapter, scoped to
/// `user_id` — a target whose prefix no longer resolves returns `None` and is simply omitted.
pub async fn resolve_target_prefix(
    cx: &AppState,
    user_id: &str,
    target_index: usize,
    target: &GroupTarget,
) -> Result<Option<ResolvedTarget>, sqlx::Error> {
    let Some(split) = split_model_id(&target.model) else { return Ok(None) };
    if let Some(provider) = ProviderId::parse(&split.prefix) {
        return Ok(Some(ResolvedTarget {
            target_index,
            provider: provider.as_str().to_string(),
            upstream_model: split.upstream_model,
            is_builtin: true,
            custom_provider: None,
            adapter: get_adapter(provider),
            account_id: target.account_id.clone(),
        }));
    }
    if let Some(row) = get_custom_provider_by_slug(cx.pool(), user_id, &split.prefix).await? {
        return Ok(Some(ResolvedTarget {
            target_index,
            provider: row.slug.clone(),
            upstream_model: split.upstream_model,
            is_builtin: false,
            adapter: custom_provider_adapter(&row),
            custom_provider: Some(row),
            account_id: target.account_id.clone(),
        }));
    }
    // Third prefix branch (docs/cli.md): builtin → custom slug → CLI slug.
    let Some(cli_row) = get_cli_provider_by_slug(cx.pool(), user_id, &split.prefix).await? else {
        return Ok(None);
    };
    Ok(Some(ResolvedTarget {
        target_index,
        provider: cli_row.slug.clone(),
        upstream_model: split.upstream_model,
        is_builtin: false,
        custom_provider: None,
        adapter: cli_provider_adapter(cx, &cli_row),
        account_id: target.account_id.clone(),
    }))
}

/// Whether this account's stored profile facts let it serve the target's model. Fails open.
fn eligible(target: &ResolvedTarget, account: &AccountRow) -> bool {
    target.adapter.supports_model(account_profile_meta(account).as_ref(), &target.upstream_model)
}

struct PoolRow {
    account: AccountRow,
    priority: i32,
    share: Option<ShareInfo>,
}

/// The viewer's own rows plus, for an **unpinned builtin** target and only when an edition
/// composed in a pool extension, the rows other users share with them (docs/cloud-edition.md
/// § "Pool extension"). Both kinds are merged by `(priority DESC, created_at DESC)` — a shared
/// row orders on the viewer's own `SharedAccount.priority`, never the owner's — and a `share`
/// descriptor marks the borrowed ones for everything downstream. Pinned targets and custom/CLI
/// providers never consult the extension.
async fn pool_rows(cx: &AppState, user_id: &str, target: &ResolvedTarget) -> Result<Vec<PoolRow>, sqlx::Error> {
    let own = list_accounts(cx.pool(), user_id, &target.provider).await?;
    let mut rows: Vec<PoolRow> =
        own.into_iter().map(|account| PoolRow { priority: account.priority, account, share: None }).collect();
    let Some(ext) = cx.pool_extension() else { return Ok(rows) };
    if !target.is_builtin || target.account_id.is_some() {
        return Ok(rows);
    }
    let Some(provider) = ProviderId::parse(&target.provider) else { return Ok(rows) };
    match ext.list_shared(cx, user_id, provider, ListSharedOptions::default()).await {
        Ok(shared) => {
            for s in shared {
                rows.push(PoolRow { account: s.account, priority: s.priority, share: Some(s.share) });
            }
        }
        Err(error) => {
            // Borrowing is an addition to the viewer's own pool; losing it must never take the
            // request down with it.
            tracing::error!(%error, "pool extension listShared failed");
            return Ok(rows);
        }
    }
    // Stable sort: rows that tie keep the order they were listed in, so the own-pool sequence
    // `list_accounts` already produced never reshuffles.
    rows.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| b.account.created_at.cmp(&a.account.created_at)));
    Ok(rows)
}

async fn pool_candidates_for(
    cx: &AppState,
    user_id: &str,
    target: &ResolvedTarget,
) -> Result<Vec<RoutingCandidate>, sqlx::Error> {
    let rows = pool_rows(cx, user_id, target).await?;
    Ok(rows
        .into_iter()
        .filter(|row| eligible(target, &row.account))
        .map(|row| RoutingCandidate {
            target_index: target.target_index,
            pinned: false,
            provider: target.provider.clone(),
            upstream_model: target.upstream_model.clone(),
            is_builtin: target.is_builtin,
            custom_provider: target.custom_provider.clone(),
            adapter: target.adapter.clone(),
            account: row.account,
            share: row.share,
        })
        .collect())
}

/// The one candidate a pinned target contributes — exactly that `upstream_accounts` row, if it
/// still exists and still belongs to the target's provider (docs/providers.md § Model groups
/// "Account pinning"). A deleted account, or one that quietly belongs to a different provider
/// than the target claims, contributes nothing: same as an empty pool for this target.
async fn pinned_candidate_for(
    cx: &AppState,
    user_id: &str,
    target: &ResolvedTarget,
) -> Result<Option<RoutingCandidate>, sqlx::Error> {
    let Some(account_id) = target.account_id.as_deref() else { return Ok(None) };
    let Some(account) = get_account(cx.pool(), user_id, account_id).await? else { return Ok(None) };
    if account.provider != target.provider || !eligible(target, &account) {
        return Ok(None);
    }
    Ok(Some(RoutingCandidate {
        target_index: target.target_index,
        pinned: true,
        provider: target.provider.clone(),
        upstream_model: target.upstream_model.clone(),
        is_builtin: target.is_builtin,
        custom_provider: target.custom_provider.clone(),
        adapter: target.adapter.clone(),
        account,
        share: None,
    }))
}

/// Candidates for one already-resolved target, honoring pinning.
pub async fn candidates_for_target(
    cx: &AppState,
    user_id: &str,
    target: &ResolvedTarget,
) -> Result<Vec<RoutingCandidate>, sqlx::Error> {
    if target.account_id.is_some() {
        return Ok(pinned_candidate_for(cx, user_id, target).await?.into_iter().collect());
    }
    pool_candidates_for(cx, user_id, target).await
}

/// One implicit target — the direct `provider/model` case, or dispatch's single-pool fallback
/// when it wasn't handed a pre-built candidate list.
#[derive(Clone)]
pub struct PoolTarget {
    pub provider: String,
    pub upstream_model: String,
    pub is_builtin: bool,
    pub custom_provider: Option<CustomProviderRow>,
    pub adapter: DynAdapter,
    /// Pins the call to one `upstream_accounts` row, same as a group target.
    pub account_id: Option<String>,
}

/// One implicit target's candidates (`target_index: 0`).
pub async fn pool_candidates(
    cx: &AppState,
    user_id: &str,
    target: &PoolTarget,
) -> Result<Vec<RoutingCandidate>, sqlx::Error> {
    let resolved = ResolvedTarget {
        target_index: 0,
        provider: target.provider.clone(),
        upstream_model: target.upstream_model.clone(),
        is_builtin: target.is_builtin,
        custom_provider: target.custom_provider.clone(),
        adapter: target.adapter.clone(),
        account_id: target.account_id.clone(),
    };
    candidates_for_target(cx, user_id, &resolved).await
}

pub struct GroupExpansion {
    pub candidates: Vec<RoutingCandidate>,
    /// The resolved targets in array order — target 0 if it resolved, else the next one that
    /// did; empty when nothing resolved (`invalid_model`).
    pub resolved_targets: Vec<ResolvedTarget>,
}

/// Expand every target of one group model, in array order, into the flat candidate list — the
/// same failover loop now runs cross-target and in-pool.
pub async fn group_model_candidates(
    cx: &AppState,
    user_id: &str,
    model_row: &ModelGroupModelRow,
) -> Result<GroupExpansion, sqlx::Error> {
    let targets = parse_group_targets(Some(&model_row.targets_json));
    let mut resolved_targets = Vec::new();
    for (i, target) in targets.iter().enumerate() {
        if let Some(resolved) = resolve_target_prefix(cx, user_id, i, target).await? {
            resolved_targets.push(resolved);
        }
    }
    let mut candidates = Vec::new();
    for target in &resolved_targets {
        candidates.extend(candidates_for_target(cx, user_id, target).await?);
    }
    Ok(GroupExpansion { candidates, resolved_targets })
}

/// The first resolved target's provider/adapter — shape/logging metadata only (native
/// Anthropic-passthrough check, count_tokens rejection, `request_logs` on the
/// invalid/no-account paths). Actual account selection across `candidates` is the routing
/// module's job at dispatch time, not this field's.
#[derive(Clone)]
pub struct PrimaryTarget {
    pub provider: String,
    pub upstream_model: String,
    pub adapter: DynAdapter,
    pub is_builtin: bool,
    pub custom_provider: Option<CustomProviderRow>,
}

/// Top-level combinator result for a raw `model` string — used by routes.
pub struct RoutingResolution {
    /// The exact client-sent string (a group endpoint's model name, or `provider/model`).
    pub raw: String,
    /// Set when the request came through a group endpoint: `<slug>/<model name>`.
    pub group_name: Option<String>,
    pub primary: PrimaryTarget,
    pub candidates: Vec<RoutingCandidate>,
    /// `model_groups.strategy` (group hit) or `provider_settings.strategy` (direct call) — raw
    /// stored value, dispatch normalizes it.
    pub strategy: String,
}

/// Shared model-id resolution for `/openai/v1` and `/anthropic`: split on the first "/", try
/// the builtin `ProviderId` union first, then a per-user `custom_providers` lookup, then a CLI
/// provider. A `model` with no "/" is `None` (`invalid_model`) — since v4 nothing bare resolves
/// on the shared bases; group models live on their own endpoints and resolve via
/// [`resolve_group_model_candidates`].
pub async fn resolve_candidates(
    cx: &AppState,
    user_id: &str,
    model: &str,
) -> Result<Option<RoutingResolution>, sqlx::Error> {
    let Some(split) = split_model_id(model) else { return Ok(None) };

    if let Some(provider) = ProviderId::parse(&split.prefix) {
        let primary = PrimaryTarget {
            provider: provider.as_str().to_string(),
            upstream_model: split.upstream_model.clone(),
            adapter: get_adapter(provider),
            is_builtin: true,
            custom_provider: None,
        };
        return Ok(Some(RoutingResolution {
            raw: split.raw,
            group_name: None,
            candidates: pool_candidates(cx, user_id, &primary_pool_target(&primary)).await?,
            strategy: get_provider_strategy(cx.pool(), user_id, provider.as_str()).await?,
            primary,
        }));
    }

    if let Some(row) = get_custom_provider_by_slug(cx.pool(), user_id, &split.prefix).await? {
        let primary = PrimaryTarget {
            provider: row.slug.clone(),
            upstream_model: split.upstream_model.clone(),
            adapter: custom_provider_adapter(&row),
            is_builtin: false,
            custom_provider: Some(row.clone()),
        };
        return Ok(Some(RoutingResolution {
            raw: split.raw,
            group_name: None,
            candidates: pool_candidates(cx, user_id, &primary_pool_target(&primary)).await?,
            strategy: get_provider_strategy(cx.pool(), user_id, &row.slug).await?,
            primary,
        }));
    }

    // Third prefix branch (docs/cli.md): a CLI provider behaves like any provider on the
    // routing surface — its one internal account row rides the same pool machinery, only the
    // adapter's transport differs.
    let Some(cli_row) = get_cli_provider_by_slug(cx.pool(), user_id, &split.prefix).await? else {
        return Ok(None);
    };
    let primary = PrimaryTarget {
        provider: cli_row.slug.clone(),
        upstream_model: split.upstream_model,
        adapter: cli_provider_adapter(cx, &cli_row),
        is_builtin: false,
        custom_provider: None,
    };
    Ok(Some(RoutingResolution {
        raw: split.raw,
        group_name: None,
        candidates: pool_candidates(cx, user_id, &primary_pool_target(&primary)).await?,
        strategy: get_provider_strategy(cx.pool(), user_id, &cli_row.slug).await?,
        primary,
    }))
}

fn primary_pool_target(primary: &PrimaryTarget) -> PoolTarget {
    PoolTarget {
        provider: primary.provider.clone(),
        upstream_model: primary.upstream_model.clone(),
        is_builtin: primary.is_builtin,
        custom_provider: primary.custom_provider.clone(),
        adapter: primary.adapter.clone(),
        account_id: None,
    }
}

/// Group-endpoint resolution (docs/api.md § Group endpoints): the route has already resolved
/// the slug to `group` (scoped to the caller — an unknown slug is the route's 404, not this
/// function's concern); the request's `model` is matched exactly against that group's model
/// names. A miss — or a hit whose targets all fail to resolve — is `None` (`invalid_model`).
pub async fn resolve_group_model_candidates(
    cx: &AppState,
    user_id: &str,
    group: &ModelGroupRow,
    model: &str,
) -> Result<Option<RoutingResolution>, sqlx::Error> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let Some(model_row) = get_group_model_by_name(cx.pool(), &group.id, trimmed).await? else {
        return Ok(None);
    };
    let GroupExpansion { candidates, resolved_targets } = group_model_candidates(cx, user_id, &model_row).await?;
    // No target's prefix resolves at all (e.g. every target pointed at a since-deleted custom
    // provider) — the model behaves as invalid_model.
    let Some(first) = resolved_targets.first() else { return Ok(None) };
    Ok(Some(RoutingResolution {
        raw: trimmed.to_string(),
        group_name: Some(format!("{}/{}", group.slug, model_row.name)),
        primary: PrimaryTarget {
            provider: first.provider.clone(),
            upstream_model: first.upstream_model.clone(),
            adapter: first.adapter.clone(),
            is_builtin: first.is_builtin,
            custom_provider: first.custom_provider.clone(),
        },
        candidates,
        strategy: group.strategy.clone(),
    }))
}

/// Row builders and test adapters shared by the routing, dispatch and catalog tests. Builtin
/// adapters are still placeholders in `providers::registry`, so eligibility and dispatch are
/// exercised with adapters defined here.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use std::sync::Arc;
    use crate::routing::types::CandidateFacts;
    use serde_json::{Map, Value};

    /// A bare `upstream_accounts` row; tests set only the fields they assert on.
    pub(crate) fn account_row(id: &str, provider: &str) -> AccountRow {
        AccountRow {
            id: id.into(),
            user_id: "user_1".into(),
            provider: provider.into(),
            external_account_id: None,
            label: None,
            custom_label: None,
            priority: 1,
            encrypted_payload: "encrypted".into(),
            account_meta_json: None,
            usage_snapshot_json: None,
            usage_fetched_at: None,
            usage_fetching_at: None,
            bench_until: None,
            bench_reason: None,
            refreshing_at: None,
            edge_strikes: 0,
            edge_strike_at: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    /// An adapter that refuses every call — enough for candidate-shape assertions.
    pub(crate) struct InertAdapter {
        pub id: String,
        /// Models this adapter's stored facts reject, keyed by a `plan_type` meta value.
        pub reject_model_for_plan: Option<(String, String)>,
    }

    #[async_trait::async_trait]
    impl crate::providers::ProviderAdapter for InertAdapter {
        fn id(&self) -> &str {
            &self.id
        }
        async fn chat_completions(
            &self,
            _: &AppState,
            _: &crate::pool::AcquiredAccount,
            _: &crate::providers::ChatCompletionRequest,
            _: &crate::providers::CallExtras,
        ) -> Result<axum::response::Response, crate::providers::AdapterError> {
            Err(crate::providers::AdapterError::Unsupported("chat_completions"))
        }
        fn supports_model(&self, meta: Option<&Map<String, Value>>, upstream_model: &str) -> bool {
            let Some((plan, model)) = self.reject_model_for_plan.as_ref() else { return true };
            if upstream_model != model {
                return true;
            }
            // Fails open on missing facts, exactly as the contract requires.
            meta.and_then(|m| m.get("plan_type")).and_then(Value::as_str) != Some(plan.as_str())
        }
    }

    pub(crate) fn inert(id: &str) -> DynAdapter {
        Arc::new(InertAdapter { id: id.into(), reject_model_for_plan: None })
    }

    pub(crate) fn candidate_with(account_id: &str, provider: &str, target_index: usize) -> RoutingCandidate {
        RoutingCandidate {
            target_index,
            pinned: false,
            provider: provider.into(),
            upstream_model: "model-x".into(),
            is_builtin: true,
            custom_provider: None,
            adapter: inert(provider),
            account: account_row(account_id, provider),
            share: None,
        }
    }

    pub(crate) fn usable() -> CandidateFacts {
        CandidateFacts { usable: true, unusable_until: None, bench_until: None, usage_window_until: None }
    }

    pub(crate) fn unusable(until: i64) -> CandidateFacts {
        CandidateFacts {
            usable: false,
            unusable_until: Some(until),
            bench_until: Some(until),
            usage_window_until: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;
    use std::sync::Arc;
    use crate::db::test_support::{skip_without_db, test_pool, test_state};
    use crate::upstream::MockTransport;
    use serde_json::{json, Value};
    use sqlx::PgPool;

    /// The fixed `user_1` the TypeScript suite used, as a real row (every table below has a
    /// foreign key to it).
    async fn seed_user(pool: &PgPool) {
        sqlx::query(
            "INSERT INTO users (id, google_sub, email, created_at, updated_at)
             VALUES ('user_1', 'sub-user-1', 'user1@example.com',
                     '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .execute(pool)
        .await
        .expect("seed user");
    }

    async fn seed_account(pool: &PgPool, user_id: &str, id: &str, provider: &str, priority: i32) {
        sqlx::query(
            "INSERT INTO upstream_accounts (id, user_id, provider, label, priority, encrypted_payload, created_at, updated_at)
             VALUES ($1, $2, $3, $3, $4, 'encrypted', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(id)
        .bind(user_id)
        .bind(provider)
        .bind(priority)
        .execute(pool)
        .await
        .expect("seed account");
    }

    async fn seed_account_meta(pool: &PgPool, user_id: &str, id: &str, provider: &str, priority: i32, meta: &str) {
        seed_account(pool, user_id, id, provider, priority).await;
        sqlx::query("UPDATE upstream_accounts SET account_meta_json = $1 WHERE id = $2")
            .bind(meta)
            .bind(id)
            .execute(pool)
            .await
            .expect("seed meta");
    }

    async fn seed_custom_provider(pool: &PgPool, user_id: &str, slug: &str, format: &str) {
        sqlx::query(
            "INSERT INTO custom_providers (id, user_id, slug, name, format, base_url, models_mode, sort_order, created_at, updated_at)
             VALUES ($1, $2, $3, $3, $4, 'https://upstream.example.com/v1', 'auto', 0,
                     '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(format!("cprov_{slug}"))
        .bind(user_id)
        .bind(slug)
        .bind(format)
        .execute(pool)
        .await
        .expect("seed custom provider");
    }

    async fn seed_group(pool: &PgPool, user_id: &str, targets: Value) -> ModelGroupRow {
        sqlx::query(
            "INSERT INTO model_groups (id, user_id, name, slug, strategy, created_at, updated_at)
             VALUES ('mgrp_1', $1, 'group', 'my-group', 'ordered', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(user_id)
        .execute(pool)
        .await
        .expect("seed group");
        sqlx::query(
            "INSERT INTO model_group_models (id, user_id, group_id, name, targets_json, created_at, updated_at)
             VALUES ('mgmodel_1', $1, 'mgrp_1', 'opus', $2, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(user_id)
        .bind(targets.to_string())
        .execute(pool)
        .await
        .expect("seed group model");
        crate::db::model_groups::get_group_by_slug(pool, user_id, "my-group")
            .await
            .expect("read group")
            .expect("group exists")
    }

    fn model_row(targets: Value) -> ModelGroupModelRow {
        ModelGroupModelRow {
            id: "mgmodel_1".into(),
            user_id: "user_1".into(),
            group_id: "mgrp_1".into(),
            name: "opus".into(),
            targets_json: targets.to_string(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    fn target(provider: &str, upstream_model: &str, account_id: Option<&str>) -> PoolTarget {
        PoolTarget {
            provider: provider.into(),
            upstream_model: upstream_model.into(),
            is_builtin: true,
            custom_provider: None,
            adapter: inert(provider),
            account_id: account_id.map(str::to_string),
        }
    }

    fn ids(candidates: &[RoutingCandidate]) -> Vec<String> {
        candidates.iter().map(|c| c.account.id.clone()).collect()
    }

    #[tokio::test]
    async fn one_candidate_per_pool_account_in_pool_priority_order() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account(state.pool(), "user_1", "acc_high", "grok", 10).await;
        seed_account(state.pool(), "user_1", "acc_low", "grok", 1).await;
        let candidates = pool_candidates(&state, "user_1", &target("grok", "grok-4.5", None)).await.unwrap();
        assert_eq!(ids(&candidates), ["acc_high", "acc_low"]);
        assert!(candidates.iter().all(|c| !c.pinned));
        assert!(candidates.iter().all(|c| c.target_index == 0));
        assert!(candidates.iter().all(|c| c.share.is_none()));
    }

    #[tokio::test]
    async fn an_empty_pool_is_an_empty_candidate_list() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        assert!(pool_candidates(&state, "user_1", &target("grok", "grok-4.5", None)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_account_id_pins_to_exactly_that_account() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account(state.pool(), "user_1", "acc_high", "grok", 10).await;
        seed_account(state.pool(), "user_1", "acc_low", "grok", 1).await;
        let candidates =
            pool_candidates(&state, "user_1", &target("grok", "grok-4.5", Some("acc_low"))).await.unwrap();
        assert_eq!(ids(&candidates), ["acc_low"]);
        assert!(candidates[0].pinned);
    }

    #[tokio::test]
    async fn an_unpinned_group_target_contributes_the_whole_pool_in_priority_order() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account(state.pool(), "user_1", "acc_high", "claude-code", 10).await;
        seed_account(state.pool(), "user_1", "acc_low", "claude-code", 1).await;
        let expansion =
            group_model_candidates(&state, "user_1", &model_row(json!(["claude-code/claude-opus-5"]))).await.unwrap();
        assert_eq!(expansion.resolved_targets.len(), 1);
        assert_eq!(ids(&expansion.candidates), ["acc_high", "acc_low"]);
    }

    #[tokio::test]
    async fn a_pinned_group_target_contributes_exactly_its_one_account() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account(state.pool(), "user_1", "acc_high", "claude-code", 10).await;
        seed_account(state.pool(), "user_1", "acc_pinned", "claude-code", 1).await;
        let expansion = group_model_candidates(
            &state,
            "user_1",
            &model_row(json!([{ "model": "claude-code/claude-opus-5", "account_id": "acc_pinned" }])),
        )
        .await
        .unwrap();
        assert_eq!(ids(&expansion.candidates), ["acc_pinned"]);
        assert!(expansion.candidates[0].pinned);
    }

    #[tokio::test]
    async fn a_mixed_group_flattens_in_target_then_pool_order() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account(state.pool(), "user_1", "acc_pinned", "claude-code", 1).await;
        seed_account(state.pool(), "user_1", "acc_grok_high", "grok", 10).await;
        seed_account(state.pool(), "user_1", "acc_grok_low", "grok", 1).await;
        let expansion = group_model_candidates(
            &state,
            "user_1",
            &model_row(json!([
                { "model": "claude-code/claude-opus-5", "account_id": "acc_pinned" },
                "grok/grok-4.5",
            ])),
        )
        .await
        .unwrap();
        let shape: Vec<(String, usize, bool)> = expansion
            .candidates
            .iter()
            .map(|c| (c.account.id.clone(), c.target_index, c.pinned))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("acc_pinned".to_string(), 0, true),
                ("acc_grok_high".to_string(), 1, false),
                ("acc_grok_low".to_string(), 1, false),
            ]
        );
    }

    #[tokio::test]
    async fn a_deleted_custom_provider_target_is_skipped_and_the_next_one_carries_on() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account(state.pool(), "user_1", "acc_cc", "claude-code", 1).await;
        let expansion = group_model_candidates(
            &state,
            "user_1",
            &model_row(json!(["gone/some-model", "claude-code/claude-opus-5"])),
        )
        .await
        .unwrap();
        assert_eq!(expansion.resolved_targets.len(), 1);
        assert_eq!(expansion.resolved_targets[0].provider, "claude-code");
        assert_eq!(ids(&expansion.candidates), ["acc_cc"]);
    }

    #[tokio::test]
    async fn a_pinned_target_whose_account_was_deleted_contributes_nothing() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account(state.pool(), "user_1", "acc_grok", "grok", 1).await;
        let expansion = group_model_candidates(
            &state,
            "user_1",
            &model_row(json!([
                { "model": "claude-code/claude-opus-5", "account_id": "acc_never_existed" },
                "grok/grok-4.5",
            ])),
        )
        .await
        .unwrap();
        // Both targets resolve (their prefixes are valid) but target 0 contributes nothing.
        assert_eq!(expansion.resolved_targets.len(), 2);
        assert_eq!(ids(&expansion.candidates), ["acc_grok"]);
    }

    #[tokio::test]
    async fn a_pinned_target_pointing_at_another_providers_account_contributes_nothing() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account(state.pool(), "user_1", "acc_grok", "grok", 1).await;
        seed_account(state.pool(), "user_1", "acc_codex", "codex", 1).await;
        let expansion = group_model_candidates(
            &state,
            "user_1",
            &model_row(json!([
                { "model": "claude-code/claude-opus-5", "account_id": "acc_grok" },
                "codex/gpt-5.2",
            ])),
        )
        .await
        .unwrap();
        assert_eq!(ids(&expansion.candidates), ["acc_codex"]);
    }

    #[tokio::test]
    async fn a_custom_provider_target_resolves_to_its_own_adapter() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_custom_provider(state.pool(), "user_1", "my-endpoint", "openai").await;
        seed_account(state.pool(), "user_1", "acc_custom", "my-endpoint", 1).await;
        let expansion =
            group_model_candidates(&state, "user_1", &model_row(json!(["my-endpoint/gpt-4o"]))).await.unwrap();
        assert_eq!(ids(&expansion.candidates), ["acc_custom"]);
        assert!(!expansion.candidates[0].is_builtin);
        assert_eq!(expansion.candidates[0].custom_provider.as_ref().unwrap().slug, "my-endpoint");
        assert_eq!(expansion.candidates[0].adapter.id(), "my-endpoint");
    }

    #[tokio::test]
    async fn no_target_resolving_is_an_empty_expansion() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        let expansion =
            group_model_candidates(&state, "user_1", &model_row(json!(["gone-1/model", "gone-2/model"])))
                .await
                .unwrap();
        assert!(expansion.resolved_targets.is_empty());
        assert!(expansion.candidates.is_empty());
    }

    #[tokio::test]
    async fn a_direct_model_resolves_to_its_pool_with_the_default_strategy() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account(state.pool(), "user_1", "acc_grok", "grok", 1).await;
        let resolved = resolve_candidates(&state, "user_1", "grok/grok-4.5").await.unwrap().unwrap();
        assert_eq!(resolved.primary.provider, "grok");
        assert_eq!(resolved.primary.upstream_model, "grok-4.5");
        assert!(resolved.primary.is_builtin);
        assert_eq!(ids(&resolved.candidates), ["acc_grok"]);
        assert_eq!(resolved.strategy, "ordered");
        assert!(resolved.group_name.is_none());
    }

    #[tokio::test]
    async fn a_group_model_spans_every_target_and_names_the_alias() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        let group =
            seed_group(state.pool(), "user_1", json!(["claude-code/claude-opus-5", "grok/grok-4.5"])).await;
        seed_account(state.pool(), "user_1", "acc_cc", "claude-code", 1).await;
        seed_account(state.pool(), "user_1", "acc_grok", "grok", 1).await;
        let resolved = resolve_group_model_candidates(&state, "user_1", &group, "opus").await.unwrap().unwrap();
        assert_eq!(resolved.primary.provider, "claude-code");
        assert_eq!(resolved.group_name.as_deref(), Some("my-group/opus"));
        assert_eq!(ids(&resolved.candidates), ["acc_cc", "acc_grok"]);
    }

    #[tokio::test]
    async fn bare_names_and_unknown_prefixes_miss_on_the_shared_bases() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_group(state.pool(), "user_1", json!(["claude-code/claude-opus-5"])).await;
        seed_account(state.pool(), "user_1", "acc_cc", "claude-code", 1).await;
        assert!(resolve_candidates(&state, "user_1", "opus").await.unwrap().is_none());
        assert!(resolve_candidates(&state, "user_1", "not-a-provider/model").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_name_the_group_does_not_define_misses() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        let group = seed_group(state.pool(), "user_1", json!(["claude-code/claude-opus-5"])).await;
        seed_account(state.pool(), "user_1", "acc_cc", "claude-code", 1).await;
        assert!(resolve_group_model_candidates(&state, "user_1", &group, "other").await.unwrap().is_none());
        assert!(resolve_group_model_candidates(&state, "user_1", &group, "  ").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_primary_is_target_zero_regardless_of_whether_it_has_accounts() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        let group =
            seed_group(state.pool(), "user_1", json!(["claude-code/claude-opus-5", "grok/grok-4.5"])).await;
        seed_account(state.pool(), "user_1", "acc_grok", "grok", 1).await;
        let resolved = resolve_group_model_candidates(&state, "user_1", &group, "opus").await.unwrap().unwrap();
        assert_eq!(resolved.primary.provider, "claude-code");
        assert_eq!(ids(&resolved.candidates), ["acc_grok"]);
    }

    /// The Claude Code Fable seat rule lives in that adapter's `supports_model`
    ///; this exercises the
    /// eligibility machinery candidates.rs owns, with an adapter that rejects one model for one
    /// stored plan.
    #[tokio::test]
    async fn an_account_whose_adapter_rejects_the_model_contributes_nothing() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account_meta(
            state.pool(),
            "user_1",
            "acc_team",
            "claude-code",
            15,
            &json!({ "plan_type": "claude_team" }).to_string(),
        )
        .await;
        seed_account_meta(
            state.pool(),
            "user_1",
            "acc_max",
            "claude-code",
            14,
            &json!({ "plan_type": "claude_max" }).to_string(),
        )
        .await;
        seed_account(state.pool(), "user_1", "acc_fresh", "claude-code", 13).await;

        let picky: DynAdapter = Arc::new(InertAdapter {
            id: "claude-code".into(),
            reject_model_for_plan: Some(("claude_team".into(), "claude-fable-5-1".into())),
        });
        let fable = PoolTarget {
            provider: "claude-code".into(),
            upstream_model: "claude-fable-5-1".into(),
            is_builtin: true,
            custom_provider: None,
            adapter: picky.clone(),
            account_id: None,
        };
        // The rejected seat drops out; a row with no stored profile fails open and stays.
        assert_eq!(ids(&pool_candidates(&state, "user_1", &fable).await.unwrap()), ["acc_max", "acc_fresh"]);

        // The same pool serves an unaffected model in full priority order.
        let sonnet = PoolTarget { upstream_model: "claude-sonnet-5".into(), ..fable.clone() };
        assert_eq!(
            ids(&pool_candidates(&state, "user_1", &sonnet).await.unwrap()),
            ["acc_team", "acc_max", "acc_fresh"]
        );

        // A pinned target pointing at an ineligible account contributes nothing.
        let pinned = PoolTarget { account_id: Some("acc_team".into()), ..fable.clone() };
        assert!(pool_candidates(&state, "user_1", &pinned).await.unwrap().is_empty());
        let pinned_ok = PoolTarget { account_id: Some("acc_team".into()), ..sonnet.clone() };
        assert_eq!(ids(&pool_candidates(&state, "user_1", &pinned_ok).await.unwrap()), ["acc_team"]);
    }

    #[tokio::test]
    async fn the_usage_snapshots_account_facts_win_over_account_meta_json() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account_meta(
            state.pool(),
            "user_1",
            "acc_upgraded",
            "claude-code",
            15,
            &json!({ "plan_type": "claude_team" }).to_string(),
        )
        .await;
        sqlx::query("UPDATE upstream_accounts SET usage_snapshot_json = $1 WHERE id = 'acc_upgraded'")
            .bind(
                json!({ "windows": [], "account": { "plan_type": "claude_max" }, "error": null, "stale": false })
                    .to_string(),
            )
            .execute(state.pool())
            .await
            .unwrap();
        let picky: DynAdapter = Arc::new(InertAdapter {
            id: "claude-code".into(),
            reject_model_for_plan: Some(("claude_team".into(), "claude-fable-5-1".into())),
        });
        let fable = PoolTarget {
            provider: "claude-code".into(),
            upstream_model: "claude-fable-5-1".into(),
            is_builtin: true,
            custom_provider: None,
            adapter: picky,
            account_id: None,
        };
        assert_eq!(ids(&pool_candidates(&state, "user_1", &fable).await.unwrap()), ["acc_upgraded"]);
    }

    #[tokio::test]
    async fn every_account_ineligible_is_an_empty_list_never_a_bench() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_user(state.pool()).await;
        seed_account_meta(
            state.pool(),
            "user_1",
            "acc_team",
            "claude-code",
            15,
            &json!({ "plan_type": "claude_team" }).to_string(),
        )
        .await;
        let picky: DynAdapter = Arc::new(InertAdapter {
            id: "claude-code".into(),
            reject_model_for_plan: Some(("claude_team".into(), "claude-fable-5-1".into())),
        });
        let fable = PoolTarget {
            provider: "claude-code".into(),
            upstream_model: "claude-fable-5-1".into(),
            is_builtin: true,
            custom_provider: None,
            adapter: picky,
            account_id: None,
        };
        assert!(pool_candidates(&state, "user_1", &fable).await.unwrap().is_empty());
        let row = crate::db::accounts::get_account(state.pool(), "user_1", "acc_team").await.unwrap().unwrap();
        assert!(row.bench_until.is_none(), "ineligibility never benches");
    }
}
