//! `request_logs` writes and the two point reads the logger takes (the storage half of
//! apps/api/src/logging/request_log.ts, docs/database.md § request_logs). Prompts,
//! completions and tokens never reach this table — only counts and costs (docs/logging.md).
//!
//! The aggregate queries behind `/api/logs` and `/api/usage` belong to those route ports
//! (apps/api/src/routes/logs.ts, usage.ts) and are deliberately not here; the per-key spend
//! window sum lives in `auth::spend_limit`, which owns its fail-open contract.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::ids::{new_id, now_iso};

/// One row to insert. Cost is computed by the caller (the price table lives in `pricing`), so
/// this module never blocks a deferred write on a price lookup.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestLogEntry {
    pub user_id: String,
    pub api_key_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub account_id: Option<String>,
    pub status_code: i32,
    pub latency_ms: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub cost: Option<f64>,
    pub error_code: Option<String>,
    /// Last upstream HTTP status observed; `None` means no upstream response headers arrived.
    pub upstream_status: Option<i32>,
    /// The model-group alias the request was addressed to, if any; `model`/`provider` always
    /// store the expanded canonical target.
    pub group_name: Option<String>,
    /// Name snapshots, so a key or account deleted later still reads by its last name.
    pub api_key_name: Option<String>,
    pub account_label: Option<String>,
    /// ISO-8601; the caller stamps it before dispatch.
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestLogRow {
    pub id: String,
    pub user_id: String,
    pub api_key_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub account_id: Option<String>,
    pub status_code: i32,
    pub latency_ms: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub error_code: Option<String>,
    pub created_at: String,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub cost: Option<f64>,
    pub group_name: Option<String>,
    pub upstream_status: Option<i32>,
    pub api_key_name: Option<String>,
    pub account_label: Option<String>,
    pub started_at: Option<String>,
}

/// Inserts one row and returns its id (`log_<32 hex>`).
pub async fn insert_request_log(db: &PgPool, entry: &RequestLogEntry) -> Result<String, sqlx::Error> {
    let id = new_id("log");
    sqlx::query(
        "INSERT INTO request_logs
         (id, user_id, api_key_id, provider, model, account_id, status_code, latency_ms,
          prompt_tokens, completion_tokens, cache_read_input_tokens, cache_creation_input_tokens,
          cost, error_code, upstream_status, group_name, api_key_name, account_label, created_at, started_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)",
    )
    .bind(&id)
    .bind(&entry.user_id)
    .bind(entry.api_key_id.as_deref())
    .bind(&entry.provider)
    .bind(&entry.model)
    .bind(entry.account_id.as_deref())
    .bind(entry.status_code)
    .bind(entry.latency_ms)
    .bind(entry.prompt_tokens)
    .bind(entry.completion_tokens)
    .bind(entry.cache_read_input_tokens)
    .bind(entry.cache_creation_input_tokens)
    .bind(entry.cost)
    .bind(entry.error_code.as_deref())
    .bind(entry.upstream_status)
    .bind(entry.group_name.as_deref())
    .bind(entry.api_key_name.as_deref())
    .bind(entry.account_label.as_deref())
    .bind(now_iso())
    .bind(&entry.started_at)
    .execute(db)
    .await?;
    Ok(id)
}

/// The key's name as it is right now, for the log's name snapshot. A key already deleted
/// simply leaves NULL.
pub async fn api_key_name(db: &PgPool, api_key_id: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT name FROM api_keys WHERE id = $1").bind(api_key_id).fetch_optional(db).await
}

/// The account's display label — the user's own name wins over the upstream one, and an
/// account already deleted leaves NULL.
pub async fn account_label(db: &PgPool, account_id: &str) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT custom_label, label FROM upstream_accounts WHERE id = $1")
            .bind(account_id)
            .fetch_optional(db)
            .await?;
    Ok(row.and_then(|(custom, label)| custom.filter(|s| !s.is_empty()).or(label.filter(|s| !s.is_empty()))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_account, insert_api_key, insert_user, skip_without_db, test_pool};
    use crate::pool::StoredCredential;

    #[tokio::test]
    async fn insert_and_name_snapshots() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "log@example.com").await;
        let (key, _) = insert_api_key(&pool, &user.id).await;
        let account = insert_account(&pool, &user.id, "codex", &StoredCredential::default()).await;

        assert_eq!(api_key_name(&pool, &key.id).await.unwrap().as_deref(), Some("test key"));
        assert_eq!(api_key_name(&pool, "key_missing").await.unwrap(), None);
        assert_eq!(account_label(&pool, &account.id).await.unwrap().as_deref(), Some("codex test"));
        crate::db::accounts::set_account_custom_label(&pool, &user.id, &account.id, Some("mine")).await.unwrap();
        assert_eq!(account_label(&pool, &account.id).await.unwrap().as_deref(), Some("mine"), "the user's own name wins");
        assert_eq!(account_label(&pool, "acc_missing").await.unwrap(), None);

        let entry = RequestLogEntry {
            user_id: user.id.clone(),
            api_key_id: Some(key.id.clone()),
            provider: "codex".into(),
            model: "codex/gpt-5".into(),
            account_id: Some(account.id.clone()),
            status_code: 200,
            latency_ms: 42,
            prompt_tokens: Some(10),
            completion_tokens: Some(3),
            cost: Some(0.25),
            api_key_name: Some("test key".into()),
            account_label: Some("mine".into()),
            started_at: "2026-01-01T00:00:00.000Z".into(),
            ..Default::default()
        };
        let id = insert_request_log(&pool, &entry).await.unwrap();
        assert!(id.starts_with("log_"));
        let row = sqlx::query_as::<_, RequestLogRow>("SELECT * FROM request_logs WHERE id = $1")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.user_id, user.id);
        assert_eq!(row.cost, Some(0.25));
        assert_eq!(row.latency_ms, 42);
        assert_eq!(row.error_code, None);
        assert_eq!(row.started_at.as_deref(), Some("2026-01-01T00:00:00.000Z"));
        assert!(row.created_at.ends_with('Z'));
    }

    #[tokio::test]
    async fn a_refusal_row_carries_no_cost() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "refuse@example.com").await;
        let entry = RequestLogEntry {
            user_id: user.id.clone(),
            provider: "unknown".into(),
            model: String::new(),
            status_code: 429,
            latency_ms: 0,
            error_code: Some("spend_limit_exceeded".into()),
            started_at: crate::ids::now_iso(),
            ..Default::default()
        };
        let id = insert_request_log(&pool, &entry).await.unwrap();
        let row = sqlx::query_as::<_, RequestLogRow>("SELECT * FROM request_logs WHERE id = $1")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.status_code, 429);
        assert_eq!(row.cost, None);
        assert_eq!(row.error_code.as_deref(), Some("spend_limit_exceeded"));
        assert_eq!(row.api_key_id, None);
    }
}
