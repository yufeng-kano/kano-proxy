//! HTTP route groups (apps/api/src/routes). `core_routes` assembles them in the order
//! `application.ts` mounts them; each group module owns its paths.

pub mod agent;
pub mod anthropic;
pub mod auth;
pub mod changelog;
pub mod cli;
pub mod cli_shared;
pub mod custom_providers;
pub mod group_endpoints;
pub mod keys;
pub mod logs;
pub mod model_groups;
pub mod models;
pub mod openai;
pub mod providers;
pub mod resolve_request;
pub mod responses;
pub mod usage;

use axum::Router;

use crate::AppState;

/// All core route groups, without state applied (so an edition can merge its own).
///
/// `/api/*` carries the admin CORS rule from `application.ts`: cookie-credentialed and locked
/// to `APP_URL`, so a page on any other origin gets no `Access-Control-Allow-Origin` header
/// and cannot read an admin response. Admin JSON is `no-store` (`http::cors`).
pub fn core_routes(state: &AppState) -> Router<AppState> {
    let admin = Router::new()
        .nest("/api/auth", auth::routes())
        .nest("/api/keys", keys::routes())
        .layer(crate::http::cors::admin_cors(&state.config().app_url))
        .layer(crate::http::cors::no_store_layer());
    Router::new().merge(admin)
}
