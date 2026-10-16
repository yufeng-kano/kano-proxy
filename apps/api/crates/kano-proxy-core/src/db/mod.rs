//! Postgres pool, migration runner and per-table query modules. Histories
//! stay separate per edition, as with D1: the core records in `core_migrations`, an edition
//! in its own table, and the core always applies first (docs/rust-server.md § Storage).

pub mod accounts;
pub mod cli;
pub mod custom_providers;
pub mod keys;
pub mod model_groups;
pub mod oauth_states;
pub mod provider_settings;
pub mod request_logs;
pub mod sessions;
pub mod test_support;
pub mod users;

pub use accounts::AccountRow;
pub use custom_providers::CustomProviderRow;

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

/// One embedded SQL file, applied once inside a transaction and recorded by name.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub name: &'static str,
    pub sql: &'static str,
}

pub const CORE_MIGRATIONS_TABLE: &str = "core_migrations";

pub const CORE_MIGRATIONS: &[Migration] = &[
    Migration { name: "0001_core_baseline", sql: include_str!("../../migrations/0001_core_baseline.sql") },
    Migration {
        name: "0002_cli_devices_delete_on_revoke",
        sql: include_str!("../../migrations/0002_cli_devices_delete_on_revoke.sql"),
    },
    Migration { name: "0003_api_keys_device", sql: include_str!("../../migrations/0003_api_keys_device.sql") },
    Migration { name: "0004_scoped_bench", sql: include_str!("../../migrations/0004_scoped_bench.sql") },
];

pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new().max_connections(16).connect(database_url).await
}

fn valid_table_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

/// Applies every migration not yet recorded in `table`, in order. Returns the names applied.
pub async fn migrate(
    pool: &PgPool,
    table: &str,
    migrations: &[Migration],
) -> anyhow::Result<Vec<&'static str>> {
    anyhow::ensure!(valid_table_name(table), "invalid migration table name {table:?}");
    sqlx::query(&format!(
        "CREATE TABLE IF NOT EXISTS {table} (name TEXT PRIMARY KEY, applied_at TIMESTAMPTZ NOT NULL DEFAULT now())"
    ))
    .execute(pool)
    .await?;
    let applied: Vec<String> = sqlx::query(&format!("SELECT name FROM {table}"))
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| r.get::<String, _>("name"))
        .collect();
    let mut done = Vec::new();
    for m in migrations {
        if applied.iter().any(|a| a == m.name) {
            continue;
        }
        let mut tx = pool.begin().await?;
        sqlx::raw_sql(m.sql).execute(&mut *tx).await?;
        sqlx::query(&format!("INSERT INTO {table} (name) VALUES ($1)"))
            .bind(m.name)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        tracing::info!(migration = m.name, table, "applied");
        done.push(m.name);
    }
    Ok(done)
}

/// Names still pending for `table`; empty means the schema is current.
pub async fn pending(pool: &PgPool, table: &str, migrations: &[Migration]) -> anyhow::Result<Vec<&'static str>> {
    anyhow::ensure!(valid_table_name(table), "invalid migration table name {table:?}");
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(table)
        .fetch_one(pool)
        .await?;
    if !exists {
        return Ok(migrations.iter().map(|m| m.name).collect());
    }
    let applied: Vec<String> = sqlx::query_scalar(&format!("SELECT name FROM {table}"))
        .fetch_all(pool)
        .await?;
    Ok(migrations.iter().filter(|m| !applied.iter().any(|a| a == m.name)).map(|m| m.name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool_at};

    /// A fresh-database run proves nothing about a deployed one: start from the 0001 schema
    /// with a live device, a revoked one and a provider the revoked one registered, upgrade,
    /// and check only the revoked row went.
    #[tokio::test]
    async fn upgrading_to_0002_drops_revoked_devices_and_keeps_the_rest() {
        let Some(pool) = test_pool_at(&CORE_MIGRATIONS[..1]).await else { return skip_without_db() };
        let user = insert_user(&pool, "upgrade@example.com").await;
        sqlx::query(
            "INSERT INTO cli_devices (id, user_id, name, refresh_token_hash, created_at, revoked_at) VALUES
               ('clidev_live', $1, 'live', 'h1', '2026-09-01T00:00:00.000Z', NULL),
               ('clidev_gone', $1, 'gone', 'h2', '2026-09-02T00:00:00.000Z', '2026-09-03T00:00:00.000Z')",
        )
        .bind(&user.id)
        .execute(&pool)
        .await
        .expect("existing devices");
        sqlx::query(
            "INSERT INTO cli_providers (id, user_id, device_id, slug, name, format, sort_order, created_at, updated_at)
             VALUES ('cliprov_1', $1, 'clidev_gone', 'box', 'Box', 'openai', 1, 'x', 'x')",
        )
        .bind(&user.id)
        .execute(&pool)
        .await
        .expect("existing provider");

        let applied = migrate(&pool, CORE_MIGRATIONS_TABLE, CORE_MIGRATIONS).await.expect("upgrade");
        assert_eq!(applied, ["0002_cli_devices_delete_on_revoke", "0003_api_keys_device", "0004_scoped_bench"]);
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM cli_devices").fetch_all(&pool).await.unwrap();
        assert_eq!(ids, ["clidev_live"]);
        let live = crate::db::cli::get_cli_device(&pool, "clidev_live").await.unwrap().expect("the row type reads");
        assert_eq!((live.name.as_str(), live.user_id.as_str()), ("live", user.id.as_str()));
        let providers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cli_providers").fetch_one(&pool).await.unwrap();
        assert_eq!(providers, 1, "a provider outlives its revoked device");
        // The user cascade still reaches the surviving device.
        sqlx::query("DELETE FROM users WHERE id = $1").bind(&user.id).execute(&pool).await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cli_devices").fetch_one(&pool).await.unwrap();
        assert_eq!(left, 0);
        assert!(pending(&pool, CORE_MIGRATIONS_TABLE, CORE_MIGRATIONS).await.unwrap().is_empty());
    }

    /// Upgrading a deployed 0002 schema: existing keys keep every column and read as
    /// web-created (`device_id` NULL), the new foreign key validates, a device deletion reaches
    /// only the keys it minted, and nothing is pending afterwards.
    #[tokio::test]
    async fn upgrading_to_0003_keeps_existing_keys_and_adds_the_device_link() {
        let Some(pool) = test_pool_at(&CORE_MIGRATIONS[..2]).await else { return skip_without_db() };
        let user = insert_user(&pool, "upgrade3@example.com").await;
        sqlx::query(
            "INSERT INTO api_keys (id, user_id, name, key_prefix, key_hash, created_at, spend_limit, spend_limit_interval, spend_limit_include_oauth)
             VALUES ('key_old', $1, 'Production', 'sk-kano-proxy-abcdef', 'hash_old', '2026-09-01T00:00:00.000Z', 12.5, 'weekly', 0)",
        )
        .bind(&user.id)
        .execute(&pool)
        .await
        .expect("existing key");
        sqlx::query(
            "INSERT INTO cli_devices (id, user_id, name, refresh_token_hash, created_at)
             VALUES ('clidev_a', $1, 'a', 'h1', '2026-09-01T00:00:00.000Z')",
        )
        .bind(&user.id)
        .execute(&pool)
        .await
        .expect("existing device");

        let applied = migrate(&pool, CORE_MIGRATIONS_TABLE, CORE_MIGRATIONS).await.expect("upgrade");
        assert_eq!(applied, ["0003_api_keys_device", "0004_scoped_bench"]);
        let old = crate::db::keys::list_keys(&pool, &user.id).await.unwrap();
        assert_eq!(old.len(), 1);
        assert_eq!((old[0].name.as_str(), old[0].spend_limit, old[0].spend_limit_interval.as_str()), ("Production", Some(12.5), "weekly"));
        assert_eq!(old[0].device_id, None);

        let minted = crate::db::keys::create_key_for_device(&pool, &user.id, Some("clidev_a"), "agent@a", None).await.unwrap();
        assert!(sqlx::query("UPDATE api_keys SET device_id = 'clidev_missing' WHERE id = $1")
            .bind(&minted.row.id)
            .execute(&pool)
            .await
            .is_err(), "the foreign key validates");
        sqlx::query("DELETE FROM cli_devices WHERE id = 'clidev_a'").execute(&pool).await.unwrap();
        let left: Vec<String> = sqlx::query_scalar("SELECT id FROM api_keys WHERE user_id = $1").bind(&user.id).fetch_all(&pool).await.unwrap();
        assert_eq!(left, ["key_old"], "only the minted key followed its device");
        assert!(pending(&pool, CORE_MIGRATIONS_TABLE, CORE_MIGRATIONS).await.unwrap().is_empty());
    }

    /// Upgrading a deployed 0003 schema: an account that is benched and carries a usage
    /// snapshot keeps both, reads back through `AccountRow` with no scoped bench, takes a
    /// scoped bench afterwards, and the user cascade still reaches it.
    #[tokio::test]
    async fn upgrading_to_0004_keeps_existing_benches_and_adds_scoped_ones() {
        let Some(pool) = test_pool_at(&CORE_MIGRATIONS[..3]).await else { return skip_without_db() };
        let user = insert_user(&pool, "upgrade4@example.com").await;
        sqlx::query(
            "INSERT INTO upstream_accounts (id, user_id, provider, label, priority, encrypted_payload,
                                            usage_snapshot_json, bench_until, bench_reason, created_at, updated_at)
             VALUES ('acc_old', $1, 'antigravity', 'old', 1, 'blob', '{\"windows\":[]}',
                     '2030-01-01T00:00:00.000Z', '429', 'x', 'x')",
        )
        .bind(&user.id)
        .execute(&pool)
        .await
        .expect("existing account");

        let applied = migrate(&pool, CORE_MIGRATIONS_TABLE, CORE_MIGRATIONS).await.expect("upgrade");
        assert_eq!(applied, ["0004_scoped_bench"]);
        let row = crate::db::accounts::get_account(&pool, &user.id, "acc_old").await.unwrap().expect("the row type reads");
        assert_eq!((row.bench_until.as_deref(), row.bench_reason.as_deref()), (Some("2030-01-01T00:00:00.000Z"), Some("429")));
        assert_eq!(row.usage_snapshot_json.as_deref(), Some("{\"windows\":[]}"));
        assert_eq!(row.bench_scopes_json, None);

        crate::pool::bench::mark_scope_benched(&pool, &user.id, "antigravity", "acc_old", "Argon", 60_000, "429", 0).await.unwrap();
        let row = crate::db::accounts::get_account(&pool, &user.id, "acc_old").await.unwrap().unwrap();
        assert!(row.bench_scopes_json.is_some());
        sqlx::query("DELETE FROM users WHERE id = $1").bind(&user.id).execute(&pool).await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM upstream_accounts").fetch_one(&pool).await.unwrap();
        assert_eq!(left, 0);
        assert!(pending(&pool, CORE_MIGRATIONS_TABLE, CORE_MIGRATIONS).await.unwrap().is_empty());
    }
}
