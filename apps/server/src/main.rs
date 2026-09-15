//! Standalone kano-proxy server: the core with no extensions (docs/rust-server.md).

use kano_proxy_core::maintenance::retention::spawn_scheduler;
use kano_proxy_core::routes::cli_shared::tunnel_registry_for;
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
    let state = AppState::builder(config, pool.clone())
        .tunnels(tunnel_registry_for(pool))
        .service_name("kano-proxy")
        .build();
    // The Worker's cron: retention, then the price table refresh, daily at 03:17 UTC.
    spawn_scheduler(state.clone(), None);
    serve(addr, build_router(state, extensions)).await
}
