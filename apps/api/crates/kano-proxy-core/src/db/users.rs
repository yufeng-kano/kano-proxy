//! `users` rows. Google OIDC is the only sign-in, so a user is
//! created or refreshed from the profile claims keyed by `google_sub` (docs/auth.md).

use sqlx::PgPool;

use crate::ids::{new_id, now_iso};

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct UserRow {
    pub id: String,
    pub google_sub: String,
    pub email: String,
    pub name: Option<String>,
    pub picture_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// The usage-notice version last accepted; `None` until the first acceptance
    /// (docs/auth.md § Usage notice).
    pub notice_version: Option<String>,
    pub notice_accepted_at: Option<String>,
}

/// The Google profile fields the upsert reads (`sub`, `email`, optional `name`/`picture`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GoogleProfile {
    pub sub: String,
    pub email: String,
    pub name: Option<String>,
    pub picture: Option<String>,
}

pub async fn find_user_by_id(db: &PgPool, id: &str) -> Result<Option<UserRow>, sqlx::Error> {
    sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Creates the user on first sign-in, otherwise refreshes the mutable profile fields, in one
/// statement keyed on the unique `google_sub`, so two concurrent first sign-ins cannot both
/// insert. `google_sub` is the identity; email and name changes upstream follow it.
pub async fn upsert_google_user(db: &PgPool, profile: &GoogleProfile) -> Result<UserRow, sqlx::Error> {
    sqlx::query_as::<_, UserRow>(
        "INSERT INTO users (id, google_sub, email, name, picture_url, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $6)
         ON CONFLICT (google_sub) DO UPDATE
           SET email = EXCLUDED.email, name = EXCLUDED.name, picture_url = EXCLUDED.picture_url,
               updated_at = EXCLUDED.updated_at
         RETURNING *",
    )
    .bind(new_id("usr"))
    .bind(&profile.sub)
    .bind(&profile.email)
    .bind(profile.name.as_deref())
    .bind(profile.picture.as_deref())
    .bind(now_iso())
    .fetch_one(db)
    .await
}

/// Records that `user_id` accepted `version`: the two columns the session gate reads and the
/// history row, in one transaction. Returns the acceptance time it stored.
pub async fn accept_notice(db: &PgPool, user_id: &str, version: &str) -> Result<String, sqlx::Error> {
    let ts = now_iso();
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE users SET notice_version = $1, notice_accepted_at = $2 WHERE id = $3")
        .bind(version)
        .bind(&ts)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    // A version accepted before (a rolled-back notice asked again) keeps its first date.
    sqlx::query(
        "INSERT INTO notice_acceptances (user_id, version, accepted_at) VALUES ($1, $2, $3)
         ON CONFLICT (user_id, version) DO NOTHING",
    )
    .bind(user_id)
    .bind(version)
    .bind(&ts)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ts)
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
        assert_eq!(find_user_by_id(&pool, "nope").await.unwrap(), None);
    }
}
