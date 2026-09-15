//! Composition-time extension points. Editions pass these to [`crate::build_router`];
//! there is no global registry, so two apps built in one process never share state.

use axum::Router;

use crate::AppState;

#[derive(Default)]
pub struct Extensions {
    /// Extra routes merged after the core routes (the TypeScript `registerRoutes`).
    pub routes: Option<Router<AppState>>,
    /// Service name reported by `GET /health`; the core reports `kano-proxy`.
    pub service_name: Option<&'static str>,
}
