//! CLI devices, login requests and CLI providers (apps/api/src/db/cli.ts, docs/cli.md).
//! The rate budget, the slug namespace guard and refresh-token rotation all ride inside their
//! statement's WHERE, so concurrent presentations cannot both win.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;

use crate::db::accounts::iso_from_ms;
use crate::db::custom_providers::MAX_CUSTOM_PROVIDERS_PER_USER;
use crate::ids::{new_id, now_iso};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct CliDeviceRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub refresh_token_hash: String,
    pub refresh_token_prev_hash: Option<String>,
    pub last_seen_at: Option<String>,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct CliLoginRequestRow {
    pub id: String,
    pub device_name: String,
    pub code_hash: Option<String>,
    pub user_id: Option<String>,
    pub expires_at: String,
    pub approved_at: Option<String>,
    pub used_at: Option<String>,
    pub attempts: i32,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct CliProviderRow {
    pub id: String,
    pub user_id: String,
    pub device_id: Option<String>,
    pub slug: String,
    pub name: String,
    /// `openai` or `anthropic`.
    pub format: String,
    pub models_json: Option<String>,
    pub models_updated_at: Option<String>,
    pub model_filter_json: Option<String>,
    pub sort_order: i32,
    pub created_at: String,
    pub updated_at: String,
}

pub const LOGIN_REQUEST_TTL_MS: i64 = 10 * 60 * 1000;
pub const MAX_LOGIN_CODE_ATTEMPTS: i32 = 5;
pub const MAX_CLI_DEVICES_PER_USER: i64 = 20;
/// Login starts allowed per IP hash inside one request-TTL window.
pub const LOGIN_STARTS_PER_IP: i64 = 10;

// ---------------------------------------------------------------------------
// Login requests

/// Creates a login request under the per-IP budget in one atomic statement: the INSERT lands
/// only while fewer than [`LOGIN_STARTS_PER_IP`] rows with this ip hash were created inside
/// the window — a read-modify-write counter would let parallel batches from one IP bypass the
/// limit (docs/cli.md § Security notes). `None` means the budget is spent.
pub async fn insert_login_request(
    db: &PgPool,
    device_name: &str,
    ip_hash: &str,
) -> Result<Option<CliLoginRequestRow>, sqlx::Error> {
    let id = new_id("clireq");
    let ts = now_iso();
    let now = crate::app::now_ms();
    let window_start = iso_from_ms(now - LOGIN_REQUEST_TTL_MS);
    let expires = iso_from_ms(now + LOGIN_REQUEST_TTL_MS);
    let r = sqlx::query(
        "INSERT INTO cli_login_requests (id, device_name, ip_hash, expires_at, attempts, created_at)
         SELECT $1::text, $2::text, $3::text, $4::text, 0, $5::text
         WHERE (SELECT COUNT(*) FROM cli_login_requests WHERE ip_hash = $3 AND created_at > $6) < $7::bigint",
    )
    .bind(&id)
    .bind(device_name)
    .bind(ip_hash)
    .bind(&expires)
    .bind(&ts)
    .bind(&window_start)
    .bind(LOGIN_STARTS_PER_IP)
    .execute(db)
    .await?;
    if r.rows_affected() == 0 {
        return Ok(None);
    }
    Ok(Some(CliLoginRequestRow {
        id,
        device_name: device_name.to_string(),
        code_hash: None,
        user_id: None,
        expires_at: expires,
        approved_at: None,
        used_at: None,
        attempts: 0,
        created_at: ts,
    }))
}

pub async fn get_login_request(db: &PgPool, id: &str) -> Result<Option<CliLoginRequestRow>, sqlx::Error> {
    sqlx::query_as::<_, CliLoginRequestRow>(
        "SELECT id, device_name, code_hash, user_id, expires_at, approved_at, used_at, attempts, created_at
         FROM cli_login_requests WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Approve stamps the session user and code hash in one write — the row is unauthenticated
/// before this.
pub async fn approve_login_request(
    db: &PgPool,
    id: &str,
    user_id: &str,
    code_hash: &str,
) -> Result<bool, sqlx::Error> {
    let now = now_iso();
    let r = sqlx::query(
        "UPDATE cli_login_requests
         SET user_id = $1, code_hash = $2, approved_at = $3
         WHERE id = $4 AND used_at IS NULL AND approved_at IS NULL AND expires_at > $3",
    )
    .bind(user_id)
    .bind(code_hash)
    .bind(&now)
    .bind(id)
    .execute(db)
    .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn delete_login_request(db: &PgPool, id: &str) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("DELETE FROM cli_login_requests WHERE id = $1").bind(id).execute(db).await?;
    Ok(r.rows_affected() > 0)
}

pub async fn record_login_code_attempt(db: &PgPool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE cli_login_requests SET attempts = attempts + 1 WHERE id = $1").bind(id).execute(db).await?;
    Ok(())
}

pub async fn mark_login_request_used(db: &PgPool, id: &str) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("UPDATE cli_login_requests SET used_at = $1 WHERE id = $2 AND used_at IS NULL")
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(r.rows_affected() > 0)
}

// ---------------------------------------------------------------------------
// Devices

pub async fn insert_cli_device(
    db: &PgPool,
    user_id: &str,
    name: &str,
    refresh_token_hash: &str,
) -> Result<CliDeviceRow, sqlx::Error> {
    let id = new_id("clidev");
    let ts = now_iso();
    sqlx::query(
        "INSERT INTO cli_devices (id, user_id, name, refresh_token_hash, refresh_token_prev_hash, last_seen_at, created_at, revoked_at)
         VALUES ($1, $2, $3, $4, NULL, NULL, $5, NULL)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(name)
    .bind(refresh_token_hash)
    .bind(&ts)
    .execute(db)
    .await?;
    Ok(CliDeviceRow {
        id,
        user_id: user_id.to_string(),
        name: name.to_string(),
        refresh_token_hash: refresh_token_hash.to_string(),
        refresh_token_prev_hash: None,
        last_seen_at: None,
        created_at: ts,
        revoked_at: None,
    })
}

pub async fn list_cli_devices(db: &PgPool, user_id: &str) -> Result<Vec<CliDeviceRow>, sqlx::Error> {
    sqlx::query_as::<_, CliDeviceRow>("SELECT * FROM cli_devices WHERE user_id = $1 ORDER BY created_at DESC")
        .bind(user_id)
        .fetch_all(db)
        .await
}

pub async fn get_cli_device(db: &PgPool, id: &str) -> Result<Option<CliDeviceRow>, sqlx::Error> {
    sqlx::query_as::<_, CliDeviceRow>("SELECT * FROM cli_devices WHERE id = $1").bind(id).fetch_optional(db).await
}

/// Quota counts only live devices — a user who revoked 20 over time is not locked out forever.
pub async fn count_cli_devices(db: &PgPool, user_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM cli_devices WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(user_id)
        .fetch_one(db)
        .await
}

pub async fn find_cli_device_by_refresh_hash(db: &PgPool, hash: &str) -> Result<Option<CliDeviceRow>, sqlx::Error> {
    sqlx::query_as::<_, CliDeviceRow>("SELECT * FROM cli_devices WHERE refresh_token_hash = $1")
        .bind(hash)
        .fetch_optional(db)
        .await
}

pub async fn find_cli_device_by_prev_refresh_hash(
    db: &PgPool,
    hash: &str,
) -> Result<Option<CliDeviceRow>, sqlx::Error> {
    sqlx::query_as::<_, CliDeviceRow>("SELECT * FROM cli_devices WHERE refresh_token_prev_hash = $1")
        .bind(hash)
        .fetch_optional(db)
        .await
}

/// Rotates atomically: the WHERE re-checks the presented hash so two concurrent presentations
/// of the same refresh token cannot both win (docs/cli.md).
pub async fn rotate_cli_device_refresh_token(
    db: &PgPool,
    device_id: &str,
    presented_hash: &str,
    new_hash: &str,
) -> Result<bool, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE cli_devices
         SET refresh_token_prev_hash = refresh_token_hash, refresh_token_hash = $1, last_seen_at = $2
         WHERE id = $3 AND refresh_token_hash = $4 AND revoked_at IS NULL",
    )
    .bind(new_hash)
    .bind(now_iso())
    .bind(device_id)
    .bind(presented_hash)
    .execute(db)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Idempotent — a second revoke changes nothing.
pub async fn revoke_cli_device(db: &PgPool, user_id: &str, device_id: &str) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("UPDATE cli_devices SET revoked_at = $1 WHERE id = $2 AND user_id = $3 AND revoked_at IS NULL")
        .bind(now_iso())
        .bind(device_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn touch_cli_device_last_seen(db: &PgPool, device_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE cli_devices SET last_seen_at = $1 WHERE id = $2")
        .bind(now_iso())
        .bind(device_id)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Providers

pub async fn list_cli_providers(db: &PgPool, user_id: &str) -> Result<Vec<CliProviderRow>, sqlx::Error> {
    sqlx::query_as::<_, CliProviderRow>(
        "SELECT * FROM cli_providers WHERE user_id = $1 ORDER BY sort_order ASC, created_at ASC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

pub async fn get_cli_provider_by_id(db: &PgPool, user_id: &str, id: &str) -> Result<Option<CliProviderRow>, sqlx::Error> {
    sqlx::query_as::<_, CliProviderRow>("SELECT * FROM cli_providers WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .fetch_optional(db)
        .await
}

/// Scoped to `user_id` — a CLI slug must never resolve cross-user.
pub async fn get_cli_provider_by_slug(db: &PgPool, user_id: &str, slug: &str) -> Result<Option<CliProviderRow>, sqlx::Error> {
    sqlx::query_as::<_, CliProviderRow>("SELECT * FROM cli_providers WHERE user_id = $1 AND slug = $2")
        .bind(user_id)
        .bind(slug)
        .fetch_optional(db)
        .await
}

pub async fn count_cli_providers(db: &PgPool, user_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM cli_providers WHERE user_id = $1").bind(user_id).fetch_one(db).await
}

#[derive(Debug, Clone)]
pub struct NewCliProvider<'a> {
    pub user_id: &'a str,
    pub device_id: Option<&'a str>,
    pub slug: &'a str,
    pub name: &'a str,
    pub format: &'a str,
    pub models_json: Option<&'a str>,
    pub model_filter_json: Option<&'a str>,
}

/// The slug namespace is shared with `custom_providers`, and a check-then-insert pair can race
/// a concurrent custom create for the same slug — so the guard rides inside the INSERT itself:
/// the row lands only while no custom provider owns the slug and the combined cap allows it.
/// `None` on that conflict; the own-table half is the `UNIQUE(user_id, slug)` constraint.
pub async fn insert_cli_provider(
    db: &PgPool,
    input: NewCliProvider<'_>,
) -> Result<Option<CliProviderRow>, sqlx::Error> {
    let id = new_id("cliprov");
    let ts = now_iso();
    let max: i32 = sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), 0) FROM cli_providers WHERE user_id = $1")
        .bind(input.user_id)
        .fetch_one(db)
        .await?;
    let sort_order = max + 1;
    let models_updated_at = input.models_json.map(|_| ts.clone());
    let r = sqlx::query(
        "INSERT INTO cli_providers
         (id, user_id, device_id, slug, name, format, models_json, models_updated_at, model_filter_json, sort_order, created_at, updated_at)
         SELECT $1::text, $2::text, $3::text, $4::text, $5::text, $6::text, $7::text, $8::text, $9::text,
                $10::int, $11::text, $11::text
         WHERE NOT EXISTS (SELECT 1 FROM custom_providers WHERE user_id = $2 AND slug = $4)
           AND ((SELECT COUNT(*) FROM cli_providers WHERE user_id = $2)
              + (SELECT COUNT(*) FROM custom_providers WHERE user_id = $2)) < $12::bigint",
    )
    .bind(&id)
    .bind(input.user_id)
    .bind(input.device_id)
    .bind(input.slug)
    .bind(input.name)
    .bind(input.format)
    .bind(input.models_json)
    .bind(models_updated_at.as_deref())
    .bind(input.model_filter_json)
    .bind(sort_order)
    .bind(&ts)
    .bind(MAX_CUSTOM_PROVIDERS_PER_USER)
    .execute(db)
    .await?;
    if r.rows_affected() == 0 {
        return Ok(None);
    }
    Ok(Some(CliProviderRow {
        id,
        user_id: input.user_id.to_string(),
        device_id: input.device_id.map(str::to_string),
        slug: input.slug.to_string(),
        name: input.name.to_string(),
        format: input.format.to_string(),
        models_json: input.models_json.map(str::to_string),
        models_updated_at,
        model_filter_json: input.model_filter_json.map(str::to_string),
        sort_order,
        created_at: ts.clone(),
        updated_at: ts,
    }))
}

pub async fn rename_cli_provider(db: &PgPool, user_id: &str, id: &str, name: &str) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("UPDATE cli_providers SET name = $1, updated_at = $2 WHERE id = $3 AND user_id = $4")
        .bind(name)
        .bind(now_iso())
        .bind(id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn delete_cli_provider(db: &PgPool, user_id: &str, id: &str) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("DELETE FROM cli_providers WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(r.rows_affected() > 0)
}

/// The agent-reported catalog — stored whole; the expose filter applies at read time only.
pub async fn write_cli_provider_models(db: &PgPool, provider_id: &str, models: &[String]) -> Result<(), sqlx::Error> {
    let ts = now_iso();
    sqlx::query("UPDATE cli_providers SET models_json = $1, models_updated_at = $2, updated_at = $2 WHERE id = $3")
        .bind(serde_json::to_string(models).expect("models serialize"))
        .bind(&ts)
        .bind(provider_id)
        .execute(db)
        .await?;
    Ok(())
}

pub fn parse_cli_models(json: Option<&str>) -> Vec<String> {
    let Some(json) = json.filter(|s| !s.is_empty()) else { return Vec::new() };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    items.into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
}

/// The reported list with the expose-whitelist applied — what every read surface shows.
pub fn exposed_cli_models(models_json: Option<&str>, model_filter_json: Option<&str>) -> Vec<String> {
    let reported = parse_cli_models(models_json);
    let filter = parse_cli_models(model_filter_json);
    if filter.is_empty() {
        return reported;
    }
    reported.into_iter().filter(|m| filter.iter().any(|f| f == m)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool};

    #[test]
    fn model_lists_parse_and_filter() {
        assert!(parse_cli_models(None).is_empty());
        assert!(parse_cli_models(Some("{")).is_empty());
        assert_eq!(parse_cli_models(Some(r#"["a",1,"b"]"#)), vec!["a", "b"]);
        assert_eq!(exposed_cli_models(Some(r#"["a","b"]"#), None), vec!["a", "b"]);
        assert_eq!(exposed_cli_models(Some(r#"["a","b"]"#), Some("[]")), vec!["a", "b"], "an empty filter exposes all");
        assert_eq!(exposed_cli_models(Some(r#"["a","b"]"#), Some(r#"["b","c"]"#)), vec!["b"]);
    }

    #[tokio::test]
    async fn login_requests_respect_the_per_ip_budget() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        for i in 0..LOGIN_STARTS_PER_IP {
            assert!(insert_login_request(&pool, &format!("dev{i}"), "ip-a").await.unwrap().is_some());
        }
        assert!(insert_login_request(&pool, "one too many", "ip-a").await.unwrap().is_none());
        assert!(insert_login_request(&pool, "other ip", "ip-b").await.unwrap().is_some(), "the budget is per ip hash");
    }

    #[tokio::test]
    async fn login_request_lifecycle() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "cli@example.com").await;
        let req = insert_login_request(&pool, "laptop", "ip").await.unwrap().unwrap();
        assert!(req.id.starts_with("clireq_"));
        assert_eq!(get_login_request(&pool, &req.id).await.unwrap().unwrap().attempts, 0);
        record_login_code_attempt(&pool, &req.id).await.unwrap();
        assert_eq!(get_login_request(&pool, &req.id).await.unwrap().unwrap().attempts, 1);

        assert!(approve_login_request(&pool, &req.id, &user.id, "code-hash").await.unwrap());
        assert!(!approve_login_request(&pool, &req.id, &user.id, "again").await.unwrap(), "approval is once");
        let stored = get_login_request(&pool, &req.id).await.unwrap().unwrap();
        assert_eq!(stored.user_id.as_deref(), Some(user.id.as_str()));
        assert_eq!(stored.code_hash.as_deref(), Some("code-hash"));

        assert!(mark_login_request_used(&pool, &req.id).await.unwrap());
        assert!(!mark_login_request_used(&pool, &req.id).await.unwrap(), "use is once");
        assert!(delete_login_request(&pool, &req.id).await.unwrap());
        assert!(!delete_login_request(&pool, &req.id).await.unwrap());
    }

    #[tokio::test]
    async fn an_expired_request_cannot_be_approved() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "expired@example.com").await;
        let req = insert_login_request(&pool, "laptop", "ip").await.unwrap().unwrap();
        sqlx::query("UPDATE cli_login_requests SET expires_at = $1 WHERE id = $2")
            .bind("2020-01-01T00:00:00.000Z")
            .bind(&req.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!approve_login_request(&pool, &req.id, &user.id, "code").await.unwrap());
    }

    #[tokio::test]
    async fn device_rotation_is_single_use() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "dev@example.com").await;
        let device = insert_cli_device(&pool, &user.id, "laptop", "hash-1").await.unwrap();
        assert_eq!(count_cli_devices(&pool, &user.id).await.unwrap(), 1);
        assert_eq!(find_cli_device_by_refresh_hash(&pool, "hash-1").await.unwrap().unwrap().id, device.id);

        assert!(rotate_cli_device_refresh_token(&pool, &device.id, "hash-1", "hash-2").await.unwrap());
        assert!(!rotate_cli_device_refresh_token(&pool, &device.id, "hash-1", "hash-3").await.unwrap(), "replay loses");
        assert_eq!(find_cli_device_by_prev_refresh_hash(&pool, "hash-1").await.unwrap().unwrap().id, device.id);
        assert!(get_cli_device(&pool, &device.id).await.unwrap().unwrap().last_seen_at.is_some());
        touch_cli_device_last_seen(&pool, &device.id).await.unwrap();

        assert!(revoke_cli_device(&pool, &user.id, &device.id).await.unwrap());
        assert!(!revoke_cli_device(&pool, &user.id, &device.id).await.unwrap(), "revoke is idempotent");
        assert_eq!(count_cli_devices(&pool, &user.id).await.unwrap(), 0, "revoked devices free the quota");
        assert_eq!(list_cli_devices(&pool, &user.id).await.unwrap().len(), 1);
        assert!(!rotate_cli_device_refresh_token(&pool, &device.id, "hash-2", "hash-4").await.unwrap(), "revoked cannot rotate");
    }

    #[tokio::test]
    async fn cli_providers_share_the_slug_namespace() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "prov@example.com").await;
        let new = |slug: &'static str| NewCliProvider {
            user_id: &user.id,
            device_id: None,
            slug,
            name: "agent",
            format: "openai",
            models_json: Some(r#"["m1","m2"]"#),
            model_filter_json: Some(r#"["m2"]"#),
        };
        let row = insert_cli_provider(&pool, new("agent")).await.unwrap().unwrap();
        assert!(row.id.starts_with("cliprov_"));
        assert_eq!(row.sort_order, 1);
        assert!(row.models_updated_at.is_some());
        assert_eq!(exposed_cli_models(row.models_json.as_deref(), row.model_filter_json.as_deref()), vec!["m2"]);
        assert_eq!(count_cli_providers(&pool, &user.id).await.unwrap(), 1);
        assert_eq!(get_cli_provider_by_slug(&pool, &user.id, "agent").await.unwrap().unwrap().id, row.id);
        assert!(get_cli_provider_by_slug(&pool, "someone-else", "agent").await.unwrap().is_none());
        assert!(get_cli_provider_by_id(&pool, &user.id, &row.id).await.unwrap().is_some());

        crate::db::custom_providers::insert_custom_provider(
            &pool,
            crate::db::custom_providers::NewCustomProvider {
                user_id: &user.id,
                slug: "taken",
                name: "custom",
                format: "openai",
                base_url: "https://example.test",
                count_tokens_url: None,
                models_mode: "auto",
                manual_models_json: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert!(insert_cli_provider(&pool, new("taken")).await.unwrap().is_none(), "a custom slug blocks the CLI insert");

        write_cli_provider_models(&pool, &row.id, &["m3".to_string()]).await.unwrap();
        let stored = get_cli_provider_by_id(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(parse_cli_models(stored.models_json.as_deref()), vec!["m3"]);
        assert!(rename_cli_provider(&pool, &user.id, &row.id, "renamed").await.unwrap());
        assert!(!rename_cli_provider(&pool, "someone-else", &row.id, "hijack").await.unwrap());
        assert_eq!(list_cli_providers(&pool, &user.id).await.unwrap()[0].name, "renamed");
        assert!(delete_cli_provider(&pool, &user.id, &row.id).await.unwrap());
    }
}
