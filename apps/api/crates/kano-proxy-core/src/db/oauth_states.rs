//! `oauth_login_states` rows: the short-lived server side of both OAuth flows — the admin
//! Google sign-in (`kind = 'google'`, [`crate::auth::google`]) and a provider login
//! (`kind = 'provider'`, [`crate::routes::providers`]). Rows expire in 15 minutes and are
//! pruned opportunistically on the same path that adds them (docs/logging.md).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::ids::now_iso;

pub const GOOGLE_KIND: &str = "google";
pub const PROVIDER_KIND: &str = "provider";

/// Login states live 15 minutes, the same budget the TypeScript used for both flows.
pub const LOGIN_STATE_TTL_MS: i64 = 900_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct OAuthLoginStateRow {
    pub id: String,
    pub kind: String,
    pub user_id: Option<String>,
    pub provider: Option<String>,
    pub payload_json: String,
    pub expires_at: String,
    pub created_at: String,
}

/// The admin sign-in state: no user yet (there may be no account), no provider.
pub async fn insert_google_state(db: &PgPool, id: &str, expires_at: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO oauth_login_states (id, kind, user_id, provider, payload_json, expires_at, created_at)
         VALUES ($1, 'google', NULL, NULL, '{}', $2, $3)",
    )
    .bind(id)
    .bind(expires_at)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn get_google_state(db: &PgPool, id: &str) -> Result<Option<OAuthLoginStateRow>, sqlx::Error> {
    sqlx::query_as::<_, OAuthLoginStateRow>("SELECT * FROM oauth_login_states WHERE id = $1 AND kind = 'google'")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// A provider login state, carrying the pending PKCE/device payload for the exchange.
pub async fn insert_provider_state(
    db: &PgPool,
    id: &str,
    user_id: &str,
    provider: &str,
    payload_json: &str,
    expires_at: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO oauth_login_states (id, kind, user_id, provider, payload_json, expires_at, created_at)
         VALUES ($1, 'provider', $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(user_id)
    .bind(provider)
    .bind(payload_json)
    .bind(expires_at)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

/// One provider state by id, scoped to its owner and provider (never cross-user).
pub async fn get_provider_state(
    db: &PgPool,
    user_id: &str,
    provider: &str,
    login_id: &str,
) -> Result<Option<OAuthLoginStateRow>, sqlx::Error> {
    sqlx::query_as::<_, OAuthLoginStateRow>(
        "SELECT * FROM oauth_login_states WHERE id = $1 AND user_id = $2 AND provider = $3",
    )
    .bind(login_id)
    .bind(user_id)
    .bind(provider)
    .fetch_optional(db)
    .await
}

/// Every live provider state for one (user, provider) — the fallback the callback paste uses
/// to match on `oauth_state` when the dialog carried the wrong login id.
pub async fn list_provider_states(
    db: &PgPool,
    user_id: &str,
    provider: &str,
) -> Result<Vec<OAuthLoginStateRow>, sqlx::Error> {
    sqlx::query_as::<_, OAuthLoginStateRow>(
        "SELECT * FROM oauth_login_states WHERE user_id = $1 AND provider = $2 AND kind = 'provider'",
    )
    .bind(user_id)
    .bind(provider)
    .fetch_all(db)
    .await
}

pub async fn delete_state(db: &PgPool, id: &str) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("DELETE FROM oauth_login_states WHERE id = $1").bind(id).execute(db).await?;
    Ok(r.rows_affected() > 0)
}

/// Opportunistic prune on the path that adds rows, ahead of the daily retention sweep.
/// Returns how many expired rows went.
pub async fn delete_expired_states(db: &PgPool, now_iso_value: &str) -> Result<u64, sqlx::Error> {
    let r = sqlx::query("DELETE FROM oauth_login_states WHERE expires_at < $1")
        .bind(now_iso_value)
        .execute(db)
        .await?;
    Ok(r.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::accounts::iso_from_ms;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool};

    #[tokio::test]
    async fn google_and_provider_states_are_separate_kinds() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "oauth@example.com").await;
        let expires = iso_from_ms(crate::app::now_ms() + LOGIN_STATE_TTL_MS);
        insert_google_state(&pool, "gstate_1", &expires).await.unwrap();
        insert_provider_state(&pool, "login_1", &user.id, "codex", r#"{"oauth_state":"s1"}"#, &expires).await.unwrap();

        let google = get_google_state(&pool, "gstate_1").await.unwrap().unwrap();
        assert_eq!(google.payload_json, "{}");
        assert_eq!(google.user_id, None);
        assert!(get_google_state(&pool, "login_1").await.unwrap().is_none(), "a provider row is not a google state");

        assert!(get_provider_state(&pool, &user.id, "codex", "login_1").await.unwrap().is_some());
        assert!(get_provider_state(&pool, "someone-else", "codex", "login_1").await.unwrap().is_none());
        assert!(get_provider_state(&pool, &user.id, "grok", "login_1").await.unwrap().is_none());
        assert_eq!(list_provider_states(&pool, &user.id, "codex").await.unwrap().len(), 1);

        assert!(delete_state(&pool, "gstate_1").await.unwrap());
        assert!(!delete_state(&pool, "gstate_1").await.unwrap());
    }

    #[tokio::test]
    async fn expired_states_are_pruned() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        insert_google_state(&pool, "old", "2020-01-01T00:00:00.000Z").await.unwrap();
        insert_google_state(&pool, "live", &iso_from_ms(crate::app::now_ms() + LOGIN_STATE_TTL_MS)).await.unwrap();
        assert_eq!(delete_expired_states(&pool, &crate::ids::now_iso()).await.unwrap(), 1);
        assert!(get_google_state(&pool, "old").await.unwrap().is_none());
        assert!(get_google_state(&pool, "live").await.unwrap().is_some());
    }
}
