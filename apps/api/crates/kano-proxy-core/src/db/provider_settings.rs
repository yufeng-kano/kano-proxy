//! `provider_settings` rows: per (user, provider) settings — the pool-level routing strategy,
//! the direct-call counterpart of `model_groups.strategy` (docs/providers.md § Routing module),
//! and the two request-edit switches (docs/providers.md § Request edits).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::ids::now_iso;

/// A missing row means `ordered`; rows are created lazily on first write, never backfilled.
/// (`routing::strategy::DEFAULT_STRATEGY` in the TypeScript; kept literal here so this module
/// does not depend on the routing port.)
pub const DEFAULT_STRATEGY: &str = "ordered";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProviderSettingsRow {
    pub user_id: String,
    pub provider: String,
    pub strategy: String,
    pub strip_fast_mode: bool,
    pub strip_claude_code_line: bool,
    pub updated_at: String,
}

/// One provider's settings as every read surface reports them: a missing row reads as
/// `ordered` with both switches off.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ProviderSettings {
    pub strategy: String,
    pub strip_fast_mode: bool,
    pub strip_claude_code_line: bool,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self { strategy: DEFAULT_STRATEGY.to_string(), strip_fast_mode: false, strip_claude_code_line: false }
    }
}

/// The columns one write sets. `None` keeps the stored value, or the column default when the
/// row does not exist yet, so a strategy write never resets a switch and vice versa.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderSettingsUpdate<'a> {
    pub strategy: Option<&'a str>,
    pub strip_fast_mode: Option<bool>,
    pub strip_claude_code_line: Option<bool>,
}

/// `provider` is a builtin id, a custom provider's slug or a CLI provider's slug.
pub async fn get_provider_settings(db: &PgPool, user_id: &str, provider: &str) -> Result<ProviderSettings, sqlx::Error> {
    let row: Option<ProviderSettings> = sqlx::query_as(
        "SELECT strategy, strip_fast_mode, strip_claude_code_line FROM provider_settings
         WHERE user_id = $1 AND provider = $2",
    )
    .bind(user_id)
    .bind(provider)
    .fetch_optional(db)
    .await?;
    Ok(row.unwrap_or_default())
}

pub async fn get_provider_strategy(db: &PgPool, user_id: &str, provider: &str) -> Result<String, sqlx::Error> {
    Ok(get_provider_settings(db, user_id, provider).await?.strategy)
}

/// Every stored row among `user_ids` × `providers`, for the dispatch walk's one settings read
/// per request. Pairs without a row are simply absent: they read as the defaults.
pub async fn list_provider_settings(
    db: &PgPool,
    user_ids: &[String],
    providers: &[String],
) -> Result<Vec<ProviderSettingsRow>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM provider_settings WHERE user_id = ANY($1) AND provider = ANY($2)")
        .bind(user_ids)
        .bind(providers)
        .fetch_all(db)
        .await
}

/// Upsert for a low-frequency admin PATCH, touching only the columns `update` sets, and
/// returning the stored row. One statement, so a same-key race cannot interleave two halves.
pub async fn update_provider_settings(
    db: &PgPool,
    user_id: &str,
    provider: &str,
    update: ProviderSettingsUpdate<'_>,
) -> Result<ProviderSettings, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO provider_settings (user_id, provider, strategy, strip_fast_mode, strip_claude_code_line, updated_at)
         VALUES ($1, $2, COALESCE($3, $7), COALESCE($4, FALSE), COALESCE($5, FALSE), $6)
         ON CONFLICT (user_id, provider) DO UPDATE SET
           strategy = COALESCE($3, provider_settings.strategy),
           strip_fast_mode = COALESCE($4, provider_settings.strip_fast_mode),
           strip_claude_code_line = COALESCE($5, provider_settings.strip_claude_code_line),
           updated_at = EXCLUDED.updated_at
         RETURNING strategy, strip_fast_mode, strip_claude_code_line",
    )
    .bind(user_id)
    .bind(provider)
    .bind(update.strategy)
    .bind(update.strip_fast_mode)
    .bind(update.strip_claude_code_line)
    .bind(now_iso())
    .bind(DEFAULT_STRATEGY)
    .fetch_one(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool};

    #[tokio::test]
    async fn a_missing_row_reads_as_the_defaults_and_writes_upsert_one_row() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "ps@example.com").await;
        assert_eq!(get_provider_settings(&pool, &user.id, "codex").await.unwrap(), ProviderSettings::default());
        let strategy = ProviderSettingsUpdate { strategy: Some("round-robin"), ..Default::default() };
        let stored = update_provider_settings(&pool, &user.id, "codex", strategy).await.unwrap();
        assert_eq!(stored.strategy, "round-robin");
        assert!(!stored.strip_fast_mode && !stored.strip_claude_code_line, "an insert takes the column defaults");
        assert_eq!(get_provider_strategy(&pool, &user.id, "codex").await.unwrap(), "round-robin");
        // A second write updates the same row rather than adding one.
        let fast = ProviderSettingsUpdate { strip_fast_mode: Some(true), ..Default::default() };
        update_provider_settings(&pool, &user.id, "codex", fast).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM provider_settings WHERE user_id = $1")
            .bind(&user.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(get_provider_strategy(&pool, &user.id, "grok").await.unwrap(), "ordered");
    }

    #[tokio::test]
    async fn a_write_keeps_every_column_it_was_not_sent() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "ps-keep@example.com").await;
        let both = ProviderSettingsUpdate { strip_fast_mode: Some(true), strip_claude_code_line: Some(true), ..Default::default() };
        update_provider_settings(&pool, &user.id, "claude-code", both).await.unwrap();
        let strategy = ProviderSettingsUpdate { strategy: Some("ordered"), ..Default::default() };
        let stored = update_provider_settings(&pool, &user.id, "claude-code", strategy).await.unwrap();
        assert!(stored.strip_fast_mode && stored.strip_claude_code_line, "a strategy write leaves the switches");
        let line = ProviderSettingsUpdate { strip_claude_code_line: Some(false), ..Default::default() };
        let stored = update_provider_settings(&pool, &user.id, "claude-code", line).await.unwrap();
        assert_eq!(
            stored,
            ProviderSettings { strategy: "ordered".into(), strip_fast_mode: true, strip_claude_code_line: false }
        );
    }

    #[tokio::test]
    async fn the_list_read_returns_only_the_stored_pairs_asked_for() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let a = insert_user(&pool, "ps-a@example.com").await;
        let b = insert_user(&pool, "ps-b@example.com").await;
        let on = ProviderSettingsUpdate { strip_fast_mode: Some(true), ..Default::default() };
        update_provider_settings(&pool, &a.id, "codex", on).await.unwrap();
        update_provider_settings(&pool, &b.id, "codex", on).await.unwrap();
        update_provider_settings(&pool, &b.id, "claude-code", on).await.unwrap();
        let rows = list_provider_settings(&pool, std::slice::from_ref(&a.id), &["codex".into(), "claude-code".into()]).await.unwrap();
        assert_eq!(rows.iter().map(|r| (r.user_id.as_str(), r.provider.as_str())).collect::<Vec<_>>(), [(a.id.as_str(), "codex")]);
    }
}
