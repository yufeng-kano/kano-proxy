//! `users` rows. Google OIDC is the only sign-in, so a user is
//! created or refreshed from the profile claims keyed by `google_sub` (docs/auth.md).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::ids::{new_id, now_iso};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct UserRow {
    pub id: String,
    pub google_sub: String,
    pub email: String,
    pub name: Option<String>,
    pub picture_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The Google profile fields the upsert reads (`sub`, `email`, optional `name`/`picture`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GoogleProfile {
    pub sub: String,
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub picture: Option<String>,
}

pub async fn find_user_by_google_sub(db: &PgPool, google_sub: &str) -> Result<Option<UserRow>, sqlx::Error> {
    sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE google_sub = $1")
        .bind(google_sub)
        .fetch_optional(db)
        .await
}

pub async fn find_user_by_id(db: &PgPool, id: &str) -> Result<Option<UserRow>, sqlx::Error> {
    sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Creates the user on first sign-in, otherwise refreshes the mutable profile fields.
/// `google_sub` is the identity; email and name changes upstream follow it.
pub async fn upsert_google_user(db: &PgPool, profile: &GoogleProfile) -> Result<UserRow, sqlx::Error> {
    let existing = find_user_by_google_sub(db, &profile.sub).await?;
    let ts = now_iso();
    if let Some(row) = existing {
        sqlx::query("UPDATE users SET email = $1, name = $2, picture_url = $3, updated_at = $4 WHERE id = $5")
            .bind(&profile.email)
            .bind(profile.name.as_deref())
            .bind(profile.picture.as_deref())
            .bind(&ts)
            .bind(&row.id)
            .execute(db)
            .await?;
        return Ok(UserRow {
            email: profile.email.clone(),
            name: profile.name.clone(),
            picture_url: profile.picture.clone(),
            updated_at: ts,
            ..row
        });
    }
    let id = new_id("usr");
    sqlx::query(
        "INSERT INTO users (id, google_sub, email, name, picture_url, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(&id)
    .bind(&profile.sub)
    .bind(&profile.email)
    .bind(profile.name.as_deref())
    .bind(profile.picture.as_deref())
    .bind(&ts)
    .bind(&ts)
    .execute(db)
    .await?;
    Ok(UserRow {
        id,
        google_sub: profile.sub.clone(),
        email: profile.email.clone(),
        name: profile.name.clone(),
        picture_url: profile.picture.clone(),
        created_at: ts.clone(),
        updated_at: ts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{skip_without_db, test_pool};

    #[tokio::test]
    async fn upsert_creates_then_refreshes_profile() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let profile = GoogleProfile { sub: "sub-1".into(), email: "a@example.com".into(), name: Some("A".into()), picture: None };
        let created = upsert_google_user(&pool, &profile).await.unwrap();
        assert!(created.id.starts_with("usr_"));
        assert_eq!(created.created_at, created.updated_at);

        let changed = GoogleProfile { sub: "sub-1".into(), email: "b@example.com".into(), name: None, picture: Some("p".into()) };
        let updated = upsert_google_user(&pool, &changed).await.unwrap();
        assert_eq!(updated.id, created.id, "google_sub is the identity");
        assert_eq!(updated.email, "b@example.com");
        assert_eq!(updated.name, None);
        assert_eq!(updated.picture_url.as_deref(), Some("p"));

        let stored = find_user_by_id(&pool, &created.id).await.unwrap().unwrap();
        assert_eq!(stored, updated);
        assert_eq!(find_user_by_google_sub(&pool, "nope").await.unwrap(), None);
        assert_eq!(find_user_by_id(&pool, "nope").await.unwrap(), None);
    }
}
