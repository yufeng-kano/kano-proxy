//! Test storage: a fresh, uniquely named Postgres database per test, migrated with the core
//! baseline (docs/rust-server.md § Verification). Replaces the TypeScript `FakeD1` double.
//!
//! Every helper returns `None` when `KANO_TEST_DATABASE_URL` (default
//! `postgres://postgres:test@127.0.0.1:55432/postgres`) is unreachable, so a machine without
//! Postgres still runs the pure-logic tests instead of failing the suite.

#![cfg(test)]

use std::sync::Arc;

use rand::RngCore;
use sqlx::PgPool;

use crate::config::CoreConfig;
use crate::crypto::token_crypto::encrypt_json;
use crate::db::accounts::AccountRow;
use crate::db::users::{GoogleProfile, UserRow};
use crate::extensions::Extensions;
use crate::ids::{new_id, now_iso};
use crate::pool::StoredCredential;
use crate::upstream::UpstreamTransport;
use crate::AppState;

/// Base64 of 32 zero-ish bytes; only ever used against throwaway test databases.
pub const TEST_TOKEN_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
pub const TEST_SESSION_SECRET: &str = "test-session-secret-not-real";
pub const TEST_APP_URL: &str = "https://app.example.com";

fn admin_url() -> String {
    std::env::var("KANO_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:test@127.0.0.1:55432/postgres".to_string())
}

/// Says the test was skipped because no test database answered. Called from a test that got
/// `None` from [`test_pool`], so CI without Postgres still passes.
pub fn skip_without_db() {
    eprintln!("skipping: no test database at KANO_TEST_DATABASE_URL");
}

/// A pool on a freshly created `kano_test_<hex>` database with the core migrations applied.
/// `None` means the server is unreachable (never a hidden failure of the migrations
/// themselves, which still panic).
pub async fn test_pool() -> Option<PgPool> {
    // A short connect timeout, so a machine with no Postgres skips in seconds instead of
    // waiting out the pool's default acquire timeout once per test.
    let admin = match sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect(&admin_url())
        .await
    {
        Ok(p) => p,
        Err(_) => return None,
    };
    let mut bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut bytes);
    let name = format!("kano_test_{}", hex::encode(bytes));
    sqlx::query(&format!("CREATE DATABASE {name}")).execute(&admin).await.expect("create test database");
    admin.close().await;

    let url = {
        let base = admin_url();
        let (head, _) = base.rsplit_once('/').expect("database url has a path");
        format!("{head}/{name}")
    };
    let pool = crate::db::connect(&url).await.expect("connect to the fresh test database");
    crate::db::migrate(&pool, crate::db::CORE_MIGRATIONS_TABLE, crate::db::CORE_MIGRATIONS)
        .await
        .expect("core migrations apply");
    Some(pool)
}

/// Configuration for tests: the secrets the crypto seams need, `APP_URL` for the admin CORS
/// rule, and a database url that is never dialed (the pool is passed in).
pub fn test_config() -> CoreConfig {
    CoreConfig {
        listen_addr: "127.0.0.1:0".parse().expect("valid addr"),
        database_url: "postgres://unused".into(),
        web_dist_dir: None,
        app_url: TEST_APP_URL.into(),
        google_redirect_uri: "https://api.example.com/api/auth/callback".into(),
        google_client_id: Some("test-google-client".into()),
        google_client_secret: Some("test-google-secret".into()),
        session_secret: Some(TEST_SESSION_SECRET.into()),
        token_encryption_key: Some(TEST_TOKEN_KEY.into()),
        cli_token_secret: Some("test-cli-token-secret".into()),
        claude_code_oauth_client_id: None,
        codex_oauth_client_id: None,
        grok_oauth_client_id: None,
        antigravity_oauth_client_id: None,
        antigravity_oauth_client_secret: None,
        antigravity_client_version: None,
        antigravity_client_build: None,
        antigravity_hub_version: None,
        request_log_retention_days: 90,
        upstream_first_byte_timeout_ms: 180_000,
        github_repo: None,
        github_token: None,
    }
}

/// An [`AppState`] on `pool` whose only upstream is `transport` — no test ever reaches a real
/// provider or Google (docs/testing.md).
pub fn test_state(pool: PgPool, transport: Arc<dyn UpstreamTransport>) -> AppState {
    AppState::builder(test_config(), pool).transport(transport).build()
}

/// The core router for `state`, with no edition extensions.
pub fn test_router(state: AppState) -> axum::Router {
    crate::build_router(state, Extensions::default())
}

pub async fn insert_user(pool: &PgPool, email: &str) -> UserRow {
    let profile = GoogleProfile { sub: format!("sub-{email}"), email: email.into(), name: Some("Test User".into()), picture: None };
    crate::db::users::upsert_google_user(pool, &profile).await.expect("insert user")
}

/// An API key row for `user_id`, returning the plaintext the client would present.
pub async fn insert_api_key(pool: &PgPool, user_id: &str) -> (crate::db::keys::ApiKeyRow, String) {
    let created = crate::db::keys::create_key(pool, user_id, "test key", None).await.expect("insert api key");
    (created.row, created.plaintext)
}

/// An `upstream_accounts` row whose payload is encrypted with the test key, so
/// `pool::acquire` decrypts it exactly as production would.
pub async fn insert_account(pool: &PgPool, user_id: &str, provider: &str, credential: &StoredCredential) -> AccountRow {
    let blob = encrypt_json(Some(TEST_TOKEN_KEY), credential).expect("encrypt test credential");
    let id = new_id("acc");
    let ts = now_iso();
    sqlx::query(
        "INSERT INTO upstream_accounts (id, user_id, provider, external_account_id, label, priority,
                                        encrypted_payload, account_meta_json, created_at, updated_at)
         VALUES ($1, $2, $3, NULL, $4, 1, $5, NULL, $6, $6)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(provider)
    .bind(format!("{provider} test"))
    .bind(&blob)
    .bind(&ts)
    .execute(pool)
    .await
    .expect("insert account");
    crate::db::accounts::get_account(pool, user_id, &id).await.expect("read back").expect("row exists")
}

/// A key carrying spend limits, used by the API-key middleware and spend-limit tests.
pub async fn insert_api_key_with_limit(
    pool: &PgPool,
    user_id: &str,
    limits: crate::db::keys::SpendLimitFields,
) -> (crate::db::keys::ApiKeyRow, String) {
    let created = crate::db::keys::create_key(pool, user_id, "limited key", Some(limits)).await.expect("insert api key");
    (created.row, created.plaintext)
}
