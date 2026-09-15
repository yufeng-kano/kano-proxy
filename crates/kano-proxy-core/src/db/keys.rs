//! `api_keys` rows (apps/api/src/db/keys.ts). Keys are stored hashed with a 20-character
//! display prefix; the plaintext exists only in the create response (docs/auth.md).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::crypto::keys::create_api_key_material;
use crate::ids::{new_id, now_iso};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct ApiKeyRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub key_prefix: String,
    pub key_hash: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    /// USD ceiling per window; `None` = unlimited (docs/pricing.md).
    pub spend_limit: Option<f64>,
    pub spend_limit_interval: String,
    /// 0/1 — whether builtin (subscription OAuth) traffic counts toward the limit.
    pub spend_limit_include_oauth: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpendLimitFields {
    pub spend_limit: Option<f64>,
    pub spend_limit_interval: String,
    pub spend_limit_include_oauth: bool,
}

pub struct CreatedKey {
    pub row: ApiKeyRow,
    pub plaintext: String,
}

pub async fn list_keys(db: &PgPool, user_id: &str) -> Result<Vec<ApiKeyRow>, sqlx::Error> {
    sqlx::query_as::<_, ApiKeyRow>(
        "SELECT id, user_id, name, key_prefix, key_hash, created_at, last_used_at,
                spend_limit, spend_limit_interval, spend_limit_include_oauth
         FROM api_keys WHERE user_id = $1 ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

pub async fn create_key(
    db: &PgPool,
    user_id: &str,
    name: &str,
    limits: Option<SpendLimitFields>,
) -> Result<CreatedKey, sqlx::Error> {
    let material = create_api_key_material();
    let id = new_id("key");
    let ts = now_iso();
    let spend_limit = limits.as_ref().and_then(|l| l.spend_limit);
    let interval = limits.as_ref().map(|l| l.spend_limit_interval.clone()).unwrap_or_else(|| "monthly".to_string());
    // No limits block at all means the column default (1), not "excluded".
    let include_oauth = match &limits {
        Some(l) => i32::from(l.spend_limit_include_oauth),
        None => 1,
    };
    sqlx::query(
        "INSERT INTO api_keys (id, user_id, name, key_prefix, key_hash, created_at, last_used_at,
                               spend_limit, spend_limit_interval, spend_limit_include_oauth)
         VALUES ($1, $2, $3, $4, $5, $6, NULL, $7, $8, $9)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(name)
    .bind(&material.prefix)
    .bind(&material.hash)
    .bind(&ts)
    .bind(spend_limit)
    .bind(&interval)
    .bind(include_oauth)
    .execute(db)
    .await?;
    Ok(CreatedKey {
        plaintext: material.plaintext,
        row: ApiKeyRow {
            id,
            user_id: user_id.to_string(),
            name: name.to_string(),
            key_prefix: material.prefix,
            key_hash: material.hash,
            created_at: ts,
            last_used_at: None,
            spend_limit,
            spend_limit_interval: interval,
            spend_limit_include_oauth: include_oauth,
        },
    })
}

/// Rename and/or replace the limit fields. Unlike the COALESCE convention elsewhere, the
/// limit fields are written verbatim when `limits` is given — `spend_limit: None` must
/// *clear* a limit, which COALESCE cannot express. `false` means no such key for this user.
pub async fn update_key(
    db: &PgPool,
    user_id: &str,
    key_id: &str,
    name: Option<&str>,
    limits: Option<SpendLimitFields>,
) -> Result<bool, sqlx::Error> {
    let existing: Option<String> = sqlx::query_scalar("SELECT id FROM api_keys WHERE id = $1 AND user_id = $2")
        .bind(key_id)
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    if existing.is_none() {
        return Ok(false);
    }
    if let Some(name) = name {
        sqlx::query("UPDATE api_keys SET name = $1 WHERE id = $2").bind(name).bind(key_id).execute(db).await?;
    }
    if let Some(limits) = limits {
        sqlx::query(
            "UPDATE api_keys SET spend_limit = $1, spend_limit_interval = $2, spend_limit_include_oauth = $3 WHERE id = $4",
        )
        .bind(limits.spend_limit)
        .bind(&limits.spend_limit_interval)
        .bind(i32::from(limits.spend_limit_include_oauth))
        .bind(key_id)
        .execute(db)
        .await?;
    }
    Ok(true)
}

pub async fn delete_key(db: &PgPool, user_id: &str, key_id: &str) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("DELETE FROM api_keys WHERE id = $1 AND user_id = $2")
        .bind(key_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn find_key_by_hash(db: &PgPool, hash: &str) -> Result<Option<ApiKeyRow>, sqlx::Error> {
    sqlx::query_as::<_, ApiKeyRow>("SELECT * FROM api_keys WHERE key_hash = $1")
        .bind(hash)
        .fetch_optional(db)
        .await
}

/// Stamps `last_used_at`; runs off the request path (a background task, the `waitUntil`).
pub async fn touch_key(db: &PgPool, key_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE api_keys SET last_used_at = $1 WHERE id = $2")
        .bind(now_iso())
        .bind(key_id)
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::hash_api_key;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool};

    #[tokio::test]
    async fn create_list_update_delete() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "k@example.com").await;
        let created = create_key(&pool, &user.id, "first", None).await.unwrap();
        assert!(created.row.id.starts_with("key_"));
        assert_eq!(created.row.spend_limit, None);
        assert_eq!(created.row.spend_limit_interval, "monthly");
        assert_eq!(created.row.spend_limit_include_oauth, 1);
        assert_eq!(created.row.key_hash, hash_api_key(&created.plaintext));

        let found = find_key_by_hash(&pool, &created.row.key_hash).await.unwrap().unwrap();
        assert_eq!(found.id, created.row.id);
        assert_eq!(found.last_used_at, None);
        touch_key(&pool, &created.row.id).await.unwrap();
        assert!(find_key_by_hash(&pool, &created.row.key_hash).await.unwrap().unwrap().last_used_at.is_some());

        // Limits are written verbatim: an explicit None clears.
        let limits = SpendLimitFields { spend_limit: Some(25.0), spend_limit_interval: "weekly".into(), spend_limit_include_oauth: false };
        assert!(update_key(&pool, &user.id, &created.row.id, Some("renamed"), Some(limits)).await.unwrap());
        let rows = list_keys(&pool, &user.id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "renamed");
        assert_eq!(rows[0].spend_limit, Some(25.0));
        assert_eq!(rows[0].spend_limit_include_oauth, 0);
        let cleared = SpendLimitFields { spend_limit: None, spend_limit_interval: "monthly".into(), spend_limit_include_oauth: true };
        update_key(&pool, &user.id, &created.row.id, None, Some(cleared)).await.unwrap();
        assert_eq!(list_keys(&pool, &user.id).await.unwrap()[0].spend_limit, None);

        // Another user's key is invisible to both update and delete.
        let other = insert_user(&pool, "other@example.com").await;
        assert!(!update_key(&pool, &other.id, &created.row.id, Some("hijack"), None).await.unwrap());
        assert!(!delete_key(&pool, &other.id, &created.row.id).await.unwrap());
        assert_eq!(list_keys(&pool, &user.id).await.unwrap()[0].name, "renamed");
        assert!(delete_key(&pool, &user.id, &created.row.id).await.unwrap());
        assert!(list_keys(&pool, &user.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_is_newest_first() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "order@example.com").await;
        let a = create_key(&pool, &user.id, "a", None).await.unwrap().row;
        let b = create_key(&pool, &user.id, "b", None).await.unwrap().row;
        // created_at has millisecond resolution; force a distinguishable order.
        sqlx::query("UPDATE api_keys SET created_at = $1 WHERE id = $2")
            .bind("2020-01-01T00:00:00.000Z")
            .bind(&a.id)
            .execute(&pool)
            .await
            .unwrap();
        let rows = list_keys(&pool, &user.id).await.unwrap();
        assert_eq!(rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), vec![b.id, a.id]);
    }
}
