//! Standalone kano-proxy server: the core with no extensions (docs/rust-server.md).

use kano_proxy_core::{build_router, db, serve, AppState, CoreConfig, Extensions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = CoreConfig::from_env()?;
    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool, db::CORE_MIGRATIONS_TABLE, db::CORE_MIGRATIONS).await?;
    let addr = config.listen_addr;
    let extensions = Extensions::default();
    let state = AppState::new(config, pool, &extensions);
    serve(addr, build_router(state, extensions)).await
}
