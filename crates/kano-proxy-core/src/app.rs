//! HTTP composition: core routes plus edition extensions, then the built web app with an
//! SPA fallback (the Pages role). Route groups are added phase by phase (docs/rust-server.md).

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::{header, HeaderValue, Method};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use sqlx::PgPool;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

use crate::config::CoreConfig;
use crate::extensions::Extensions;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub config: CoreConfig,
    pub pool: PgPool,
    pub service_name: &'static str,
}

impl AppState {
    pub fn new(config: CoreConfig, pool: PgPool, service_name: &'static str) -> Self {
        Self { inner: Arc::new(Inner { config, pool, service_name }) }
    }
    pub fn config(&self) -> &CoreConfig {
        &self.inner.config
    }
    pub fn pool(&self) -> &PgPool {
        &self.inner.pool
    }
}

async fn health(axum::extract::State(state): axum::extract::State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true, "service": state.inner.service_name }))
}

/// Builds the application router. `web_dist_dir` (when set) is served for every path the
/// API does not own, with `index.html` as the SPA fallback.
pub fn build_router(state: AppState, extensions: Extensions) -> Router {
    let health_cors = CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]);
    let mut router = Router::new().route("/health", get(health).layer(health_cors));
    if let Some(extra) = extensions.routes {
        router = router.merge(extra);
    }
    let router = router.with_state(state.clone());
    match state.config().web_dist_dir.clone() {
        Some(dir) => router.fallback_service(spa_service(dir)),
        None => router,
    }
}

fn spa_service(dir: PathBuf) -> ServeDir<ServeFile> {
    let index = dir.join("index.html");
    ServeDir::new(dir).append_index_html_on_directories(true).fallback(ServeFile::new(index))
}

/// `Cache-Control: no-store` for JSON API responses is applied per route group; static
/// assets keep the file server defaults.
#[allow(dead_code)]
fn no_store() -> (header::HeaderName, HeaderValue) {
    (header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
}

/// Binds `addr` and serves `router` until SIGINT/SIGTERM, so editions need no direct
/// dependency on the HTTP stack.
pub async fn serve(addr: std::net::SocketAddr, router: Router) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, router).with_graceful_shutdown(shutdown_signal()).await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
    tracing::info!("shutting down");
}
