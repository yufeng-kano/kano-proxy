//! Port of apps/api/src/routes/cli_shared.ts (docs/cli.md).
//!
//! Shared between the token-authenticated `/agent/v1` surface ([`super::agent`])
//! and the session-authenticated `/api/cli` surface ([`super::cli`]): the provider
//! list item (with the tunnel's connection read-through) and the delete path both
//! exist on both surfaces.
//!
//! This module also carries the database implementations of the two tunnel
//! callbacks the Durable Object used to perform inline
//! (apps/api/src/do/agent_tunnel.ts): persisting an agent-reported catalog and
//! clearing the provider's bench on connect. [`tunnel_registry_for`] wires both
//! into a [`TunnelRegistry`] for `AppState::builder().tunnels(...)`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::db::accounts::{delete_accounts_for_provider, list_accounts};
use crate::db::cli::{
    delete_cli_provider, exposed_cli_models, list_cli_devices, list_cli_providers, parse_cli_models,
    write_cli_provider_models, CliProviderRow,
};
use crate::pool::bench::clear_bench;
use crate::tunnel::registry::{ConnectObserver, ModelsSink, TunnelRegistry};
use crate::AppState;

/// Persists `cli_providers.models_json` / `models_updated_at` (docs/cli.md § Model
/// catalog). An error propagates so the registry closes `4008` retryably and the
/// reconnect re-reports from a clean slate.
struct DbModelsSink {
    pool: PgPool,
}

#[async_trait]
impl ModelsSink for DbModelsSink {
    async fn persist_models(&self, provider_id: &str, models: Vec<String>) -> anyhow::Result<()> {
        write_cli_provider_models(&self.pool, provider_id, &models).await?;
        Ok(())
    }
}

/// Reconnect clears the bench on the provider's internal account rows: opening the
/// laptop restores service on the next request, no operator action
/// (docs/cli.md § Failover semantics). Best-effort, exactly as the DO's try/catch.
struct BenchClearObserver {
    pool: PgPool,
}

#[async_trait]
impl ConnectObserver for BenchClearObserver {
    async fn on_connect(&self, user_id: &str, slug: &str, provider_id: &str) {
        if let Err(error) = clear_provider_bench(&self.pool, user_id, slug).await {
            tracing::error!(provider_id, error = %error, "bench clear on connect failed");
        }
    }
}

async fn clear_provider_bench(pool: &PgPool, user_id: &str, slug: &str) -> Result<(), sqlx::Error> {
    for account in list_accounts(pool, user_id, slug).await? {
        clear_bench(pool, user_id, slug, &account.id).await?;
    }
    Ok(())
}

/// The tunnel registry an edition passes to `AppState::builder().tunnels(...)`:
/// the in-process replacement for the AgentTunnel Durable Object, with its two
/// database callbacks attached.
pub fn tunnel_registry_for(pool: PgPool) -> TunnelRegistry {
    TunnelRegistry::new()
        .with_models_sink(Arc::new(DbModelsSink { pool: pool.clone() }))
        .with_connect_observer(Arc::new(BenchClearObserver { pool }))
}

/// One provider's wire shape on both surfaces.
///
/// `account_id` is the internal pool-state row's id — the handle the Groups picker
/// pins a target to, exactly like a custom endpoint's key row (docs/admin-ui.md
/// § Groups page). Not a user-facing account; it carries no credential.
pub async fn cli_provider_list_item(
    state: &AppState,
    row: &CliProviderRow,
    device_names: &HashMap<String, String>,
) -> Result<Value, sqlx::Error> {
    let accounts = list_accounts(state.pool(), &row.user_id, &row.slug).await?;
    Ok(json!({
        "id": row.id,
        "slug": row.slug,
        "name": row.name,
        "format": row.format,
        "connected": state.tunnels().is_connected(&row.id),
        "account_id": accounts.first().map(|a| a.id.clone()),
        "models": exposed_cli_models(row.models_json.as_deref(), row.model_filter_json.as_deref()),
        "models_reported": parse_cli_models(row.models_json.as_deref()).len(),
        "model_filter": parse_cli_models(row.model_filter_json.as_deref()),
        "models_updated_at": row.models_updated_at,
        "device_id": row.device_id,
        "device_name": row.device_id.as_ref().and_then(|id| device_names.get(id).cloned()),
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    }))
}

pub async fn list_cli_provider_items(state: &AppState, user_id: &str) -> Result<Vec<Value>, sqlx::Error> {
    let rows = list_cli_providers(state.pool(), user_id).await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    // One query for every device name rather than one per provider — a provider's
    // device_id always belongs to the same user, so the user's device list covers
    // them all (revoked included).
    let device_names: HashMap<String, String> = list_cli_devices(state.pool(), user_id)
        .await?
        .into_iter()
        .map(|device| (device.id, device.name))
        .collect();
    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        items.push(cli_provider_list_item(state, row, &device_names).await?);
    }
    Ok(items)
}

/// Provider + internal account rows + live socket, in that order.
///
/// The TypeScript also cleared each removed account's bench key, because a bench
/// was a KV entry that outlived its row. Here the bench is two columns on the
/// account row itself, so deleting the row is the clear — there is nothing left to
/// sweep (docs/providers.md).
pub async fn remove_cli_provider(
    state: &AppState,
    user_id: &str,
    row: &CliProviderRow,
) -> Result<(), sqlx::Error> {
    delete_accounts_for_provider(state.pool(), user_id, &row.slug).await?;
    delete_cli_provider(state.pool(), user_id, &row.id).await?;
    // An unreachable socket has nothing to close; `close` is a no-op then.
    state.tunnels().close(&row.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::accounts::{insert_account, NewAccount};
    use crate::db::cli::{get_cli_provider_by_id, insert_cli_device, insert_cli_provider, NewCliProvider};
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_state};
    use crate::pool::bench::{is_benched, mark_benched};
    use crate::upstream::MockTransport;

    /// The db-backed `ModelsSink`: an agent report lands in `models_json` and stamps
    /// `models_updated_at` (the DO's `onModelsReport`).
    #[tokio::test]
    async fn the_models_sink_persists_an_agent_report() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "sink@example.com").await;
        let row = insert_cli_provider(
            &pool,
            NewCliProvider {
                user_id: &user.id,
                device_id: None,
                slug: "my-mac",
                name: "My Mac",
                format: "openai",
                models_json: None,
                model_filter_json: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert!(row.models_updated_at.is_none());

        let sink = DbModelsSink { pool: pool.clone() };
        sink.persist_models(&row.id, vec!["llama3".into(), "qwen3".into()]).await.unwrap();
        let stored = get_cli_provider_by_id(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(parse_cli_models(stored.models_json.as_deref()), vec!["llama3", "qwen3"]);
        assert!(stored.models_updated_at.is_some());

        // An unknown provider id writes nothing and is not an error — the socket
        // stays open rather than closing retryably forever.
        sink.persist_models("cliprov_missing", vec!["x".into()]).await.unwrap();
    }

    /// The db-backed `ConnectObserver`: reconnect clears the provider's bench.
    #[tokio::test]
    async fn connecting_clears_the_providers_bench() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "observer@example.com").await;
        let account = insert_account(
            &pool,
            NewAccount { user_id: &user.id, provider: "my-mac", encrypted_payload: "blob", ..Default::default() },
        )
        .await
        .unwrap();
        let now = crate::app::now_ms();
        mark_benched(&pool, &user.id, "my-mac", &account.id, Some(60_000), Some("offline"), now).await.unwrap();
        assert!(is_benched(&pool, &user.id, &account.id, now).await.unwrap());

        BenchClearObserver { pool: pool.clone() }.on_connect(&user.id, "my-mac", "cliprov_1").await;
        assert!(!is_benched(&pool, &user.id, &account.id, now).await.unwrap());

        // A slug with no accounts at all is a no-op, never an error.
        BenchClearObserver { pool: pool.clone() }.on_connect(&user.id, "nothing", "cliprov_2").await;
    }

    #[tokio::test]
    async fn the_list_item_carries_the_device_name_and_the_exposed_catalog() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "item@example.com").await;
        let device = insert_cli_device(state.pool(), &user.id, "my-mac", "hash").await.unwrap();
        let row = insert_cli_provider(
            state.pool(),
            NewCliProvider {
                user_id: &user.id,
                device_id: Some(&device.id),
                slug: "my-mac",
                name: "My Mac",
                format: "openai",
                models_json: Some(r#"["a","b","hidden"]"#),
                model_filter_json: Some(r#"["a","b"]"#),
            },
        )
        .await
        .unwrap()
        .unwrap();
        let account = insert_account(
            state.pool(),
            NewAccount { user_id: &user.id, provider: "my-mac", encrypted_payload: "blob", ..Default::default() },
        )
        .await
        .unwrap();

        let items = list_cli_provider_items(&state, &user.id).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["slug"], "my-mac");
        assert_eq!(items[0]["device_name"], "my-mac");
        assert_eq!(items[0]["account_id"], account.id);
        assert_eq!(items[0]["models"], json!(["a", "b"]), "the expose filter applies at read time");
        assert_eq!(items[0]["models_reported"], 3);
        assert_eq!(items[0]["connected"], false, "no socket is a disconnected provider");

        remove_cli_provider(&state, &user.id, &row).await.unwrap();
        assert!(get_cli_provider_by_id(state.pool(), &user.id, &row.id).await.unwrap().is_none());
        assert!(list_accounts(state.pool(), &user.id, "my-mac").await.unwrap().is_empty());
        assert!(list_cli_provider_items(&state, &user.id).await.unwrap().is_empty());
    }
}
