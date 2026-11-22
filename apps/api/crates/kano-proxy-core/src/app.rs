//! HTTP composition: core routes plus edition extensions, then the built web app with an
//! SPA fallback (docs/rust-server.md).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::http::Method;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use sqlx::PgPool;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

use crate::auth::notice::UsageNotice;
use crate::cache::Cache;
use crate::config::CoreConfig;
use crate::extensions::{Extensions, RequestPolicy};
use crate::pool::PoolExtension;
use crate::tunnel::registry::TunnelRegistry;
use crate::upstream::{ReqwestTransport, UpstreamTransport};

/// Everything a handler or adapter needs: configuration, storage, the upstream transport and
/// the edition's extensions. Cheap to clone; shared through `Arc`.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    config: CoreConfig,
    pool: PgPool,
    transport: Arc<dyn UpstreamTransport>,
    cache: Cache,
    request_policy: Option<Arc<dyn RequestPolicy>>,
    pool_extension: Option<Arc<dyn PoolExtension>>,
    tunnels: TunnelRegistry,
    service_name: &'static str,
    /// The usage notice signed-in users must have accepted (docs/auth.md § Usage notice).
    notice: UsageNotice,
    /// Whether request handling may spawn deferred work that reaches upstreams (the background
    /// usage refresh). Tests disable it so their mock transports see exactly the requests the
    /// handler itself made.
    background_work: bool,
}

/// Builder for [`AppState`]; editions set their extensions on it, tests swap the transport.
pub struct AppStateBuilder {
    config: CoreConfig,
    pool: PgPool,
    transport: Option<Arc<dyn UpstreamTransport>>,
    request_policy: Option<Arc<dyn RequestPolicy>>,
    pool_extension: Option<Arc<dyn PoolExtension>>,
    tunnels: Option<TunnelRegistry>,
    service_name: &'static str,
    terms_date: Option<&'static str>,
    background_work: bool,
}

impl AppStateBuilder {
    pub fn new(config: CoreConfig, pool: PgPool) -> Self {
        Self { config, pool, transport: None, request_policy: None, pool_extension: None, tunnels: None, service_name: "kano-proxy", terms_date: None, background_work: true }
    }
    pub fn transport(mut self, transport: Arc<dyn UpstreamTransport>) -> Self {
        self.transport = Some(transport);
        self
    }
    pub fn request_policy(mut self, policy: Option<Arc<dyn RequestPolicy>>) -> Self {
        self.request_policy = policy;
        self
    }
    pub fn pool_extension(mut self, ext: Option<Arc<dyn PoolExtension>>) -> Self {
        self.pool_extension = ext;
        self
    }
    /// The CLI tunnel registry; call its builders before passing it here (docs/cli.md).
    pub fn tunnels(mut self, tunnels: TunnelRegistry) -> Self {
        self.tunnels = Some(tunnels);
        self
    }
    pub fn service_name(mut self, name: &'static str) -> Self {
        self.service_name = name;
        self
    }
    /// The edition's terms date, `YYYY-MM-DD`; `build` panics on any other shape.
    pub fn terms_date(mut self, date: Option<&'static str>) -> Self {
        self.terms_date = date;
        self
    }
    pub fn background_work(mut self, enabled: bool) -> Self {
        self.background_work = enabled;
        self
    }
    pub fn build(self) -> AppState {
        let timeout = Duration::from_millis(self.config.upstream_first_byte_timeout_ms);
        AppState {
            inner: Arc::new(Inner {
                transport: self.transport.unwrap_or_else(|| Arc::new(ReqwestTransport::new(timeout))),
                cache: Cache::new(),
                config: self.config,
                pool: self.pool,
                request_policy: self.request_policy,
                pool_extension: self.pool_extension,
                tunnels: self.tunnels.unwrap_or_default(),
                service_name: self.service_name,
                notice: UsageNotice::new(self.terms_date),
                background_work: self.background_work,
            }),
        }
    }
}

impl AppState {
    pub fn builder(config: CoreConfig, pool: PgPool) -> AppStateBuilder {
        AppStateBuilder::new(config, pool)
    }
    pub fn notice(&self) -> &UsageNotice {
        &self.inner.notice
    }
    pub fn config(&self) -> &CoreConfig {
        &self.inner.config
    }
    pub fn pool(&self) -> &PgPool {
        &self.inner.pool
    }
    pub fn transport(&self) -> &Arc<dyn UpstreamTransport> {
        &self.inner.transport
    }
    pub fn cache(&self) -> &Cache {
        &self.inner.cache
    }
    pub fn request_policy(&self) -> Option<&Arc<dyn RequestPolicy>> {
        self.inner.request_policy.as_ref()
    }
    pub fn pool_extension(&self) -> Option<&Arc<dyn PoolExtension>> {
        self.inner.pool_extension.as_ref()
    }
    pub fn tunnels(&self) -> &TunnelRegistry {
        &self.inner.tunnels
    }
    pub fn background_work(&self) -> bool {
        self.inner.background_work
    }
    /// Epoch milliseconds now.
    pub fn now_ms(&self) -> i64 {
        now_ms()
    }
}

pub fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// Epoch seconds now — the `created` / `created_at` of a converted response.
pub fn now_seconds() -> i64 {
    now_ms().div_euclid(1000)
}

async fn health(axum::extract::State(state): axum::extract::State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true, "service": state.inner.service_name }))
}

/// Builds the application router: core route groups, then the edition's routes, then the
/// built web app (when `web_dist_dir` is set) with `index.html` as the SPA fallback.
pub fn build_router(state: AppState, extensions: Extensions) -> Router {
    let health_cors = CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]);
    let mut router = Router::new().route("/health", get(health).layer(health_cors));
    router = router.merge(crate::routes::core_routes(&state, &extensions.shadowed_paths));
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

/// Binds `addr` and serves `router` until SIGINT/SIGTERM, so editions need no direct
/// dependency on the HTTP stack.
pub async fn serve(addr: std::net::SocketAddr, router: Router) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
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
