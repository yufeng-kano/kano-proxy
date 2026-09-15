//! `sessions` rows (the storage half of apps/api/src/auth/session.ts). The row carries the
//! 14-day expiry; the cookie carries the signature (`crypto::session`).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::db::accounts::iso_from_ms;
use crate::ids::{new_id, now_iso};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct SessionRow {
    pub id: String,
    pub user_id: String,
    pub expires_at: String,
    pub created_at: String,
}

/// Inserts a session for `user_id` and returns its id (`sess_<32 hex>`).
pub async fn insert_session(db: &PgPool, user_id: &str, expires_at: &str) -> Result<String, sqlx::Error> {
    let id = new_id("sess");
    sqlx::query("INSERT INTO sessions (id, user_id, expires_at, created_at) VALUES ($1, $2, $3, $4)")
        .bind(&id)
        .bind(user_id)
        .bind(expires_at)
        .bind(now_iso())
        .execute(db)
        .await?;
    Ok(id)
}

/// The `(user_id, expires_at)` pair the session loader checks; `None` when no such row.
pub async fn get_session(db: &PgPool, session_id: &str) -> Result<Option<SessionRow>, sqlx::Error> {
    sqlx::query_as::<_, SessionRow>("SELECT id, user_id, expires_at, created_at FROM sessions WHERE id = $1")
        .bind(session_id)
        .fetch_optional(db)
        .await
}

pub async fn delete_session(db: &PgPool, session_id: &str) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("DELETE FROM sessions WHERE id = $1").bind(session_id).execute(db).await?;
    Ok(r.rows_affected() > 0)
}

/// `expires_at` for a session created `now_ms`, 14 days out (`crypto::session::SESSION_DAYS`).
pub fn expiry_from(now_ms: i64) -> String {
    iso_from_ms(now_ms + crate::crypto::session::SESSION_DAYS * 86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool};

    #[tokio::test]
    async fn insert_read_delete() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "sess@example.com").await;
        let expires = expiry_from(crate::app::now_ms());
        let id = insert_session(&pool, &user.id, &expires).await.unwrap();
        assert!(id.starts_with("sess_"));
        let row = get_session(&pool, &id).await.unwrap().unwrap();
        assert_eq!(row.user_id, user.id);
        assert_eq!(row.expires_at, expires);
        assert!(delete_session(&pool, &id).await.unwrap());
        assert!(get_session(&pool, &id).await.unwrap().is_none());
        assert!(!delete_session(&pool, &id).await.unwrap());
    }

    #[tokio::test]
    async fn deleting_a_user_cascades_to_their_sessions() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "cascade@example.com").await;
        let id = insert_session(&pool, &user.id, &expiry_from(crate::app::now_ms())).await.unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(&user.id).execute(&pool).await.unwrap();
        assert!(get_session(&pool, &id).await.unwrap().is_none());
    }
}
