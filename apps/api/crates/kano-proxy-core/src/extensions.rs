//! Composition-time extension points.
//! Editions pass these to [`crate::build_router`]; there is no global registry, so two apps
//! built in one process never share state.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;

use crate::pool::PoolExtension;
use crate::AppState;

/// Runs after API-key authentication on every model-request surface (`POST` chat
/// completions, responses, messages, audio transcriptions, count_tokens, global and group
/// endpoints). The identity is in the request extensions as [`ApiKeyIdentity`]. The policy
/// may answer without calling `next` (402/429/503), or wrap the response — for SSE it
/// observes the stream to settle once the outcome is known, exactly as the TypeScript
/// middleware did.
#[async_trait]
pub trait RequestPolicy: Send + Sync {
    async fn handle(&self, cx: AppState, req: Request, next: Next) -> Response;
}

/// The caller established by the API-key middleware (`apiKeyUserId` / `apiKeyId`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyIdentity {
    pub user_id: String,
    pub api_key_id: String,
}

#[derive(Default)]
pub struct Extensions {
    /// Extra routes merged after the core routes (the TypeScript `registerRoutes`).
    pub routes: Option<Router<AppState>>,
    /// Core paths the edition serves itself instead (the hosted `/api/changelog`); the core
    /// skips mounting them. Only paths `routes::core_routes` knows how to skip take effect.
    pub shadowed_paths: Vec<String>,
    pub request_policy: Option<Arc<dyn RequestPolicy>>,
    pub pool_extension: Option<Arc<dyn PoolExtension>>,
    /// Service name reported by `GET /health`; the core reports `kano-proxy`.
    pub service_name: Option<&'static str>,
}
