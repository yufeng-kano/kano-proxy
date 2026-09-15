//! Postgres pool, migration runner and per-table query modules (apps/api/src/db). Histories
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

pub const CORE_MIGRATIONS: &[Migration] = &[Migration {
    name: "0001_core_baseline",
    sql: include_str!("../../migrations/0001_core_baseline.sql"),
}];

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

