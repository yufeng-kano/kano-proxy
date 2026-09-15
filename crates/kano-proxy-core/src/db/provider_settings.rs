//! `provider_settings` rows (apps/api/src/db/provider_settings.ts): the pool-level routing
//! strategy for one (user, provider) — the direct-call counterpart of `model_groups.strategy`
//! (docs/providers.md § Routing module).

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
    pub updated_at: String,
}

/// `provider` is a builtin id or a custom provider's slug; a row for a deleted slug is simply
/// never read again, mirroring how `upstream_accounts` rows for a removed custom provider are
/// handled.
pub async fn get_provider_strategy(db: &PgPool, user_id: &str, provider: &str) -> Result<String, sqlx::Error> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT strategy FROM provider_settings WHERE user_id = $1 AND provider = $2")
            .bind(user_id)
            .bind(provider)
            .fetch_optional(db)
            .await?;
    Ok(row.unwrap_or_else(|| DEFAULT_STRATEGY.to_string()))
}

/// Upsert for a low-frequency admin PATCH. Postgres can express this as one statement, so
/// unlike the TypeScript's UPDATE-then-INSERT pair (written around D1's test double) a
/// same-key race cannot run both halves.
pub async fn set_provider_strategy(
    db: &PgPool,
    user_id: &str,
    provider: &str,
    strategy: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO provider_settings (user_id, provider, strategy, updated_at) VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id, provider) DO UPDATE SET strategy = EXCLUDED.strategy, updated_at = EXCLUDED.updated_at",
    )
    .bind(user_id)
    .bind(provider)
    .bind(strategy)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool};

    #[tokio::test]
    async fn missing_row_is_ordered_and_writes_upsert() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "ps@example.com").await;
        assert_eq!(get_provider_strategy(&pool, &user.id, "codex").await.unwrap(), "ordered");
        set_provider_strategy(&pool, &user.id, "codex", "round-robin").await.unwrap();
        assert_eq!(get_provider_strategy(&pool, &user.id, "codex").await.unwrap(), "round-robin");
        // A second write updates the same row rather than adding one.
        set_provider_strategy(&pool, &user.id, "codex", "ordered").await.unwrap();
        assert_eq!(get_provider_strategy(&pool, &user.id, "codex").await.unwrap(), "ordered");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM provider_settings WHERE user_id = $1")
            .bind(&user.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(get_provider_strategy(&pool, &user.id, "grok").await.unwrap(), "ordered");
    }
}
