//! Composition-time extension points. Editions pass routes to [`crate::build_router`] and the
//! request policy and pool extension to [`crate::app::AppStateBuilder`]; there is no global
//! registry, so two apps built in one process never share state.

use async_trait::async_trait;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;

use crate::AppState;

/// Runs after API-key authentication on every model-request surface (`POST` chat
/// completions, responses, messages, audio transcriptions, count_tokens, global and group
/// endpoints). The identity is in the request extensions as [`ApiKeyIdentity`]. The policy
/// may answer without calling `next` (402/429/503), or wrap the response — for SSE it
/// observes the stream to settle once the outcome is known.
#[async_trait]
pub trait RequestPolicy: Send + Sync {
    async fn handle(&self, cx: AppState, req: Request, next: Next) -> Response;
}

/// The caller established by the API-key middleware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyIdentity {
    pub user_id: String,
    pub api_key_id: String,
}

#[derive(Default)]
pub struct Extensions {
    /// Extra routes merged after the core routes.
    pub routes: Option<Router<AppState>>,
    /// Core paths the edition serves itself instead (the hosted `/api/changelog`); the core
    /// skips mounting them. Only paths `routes::core_routes` knows how to skip take effect.
    pub shadowed_paths: Vec<String>,
}
