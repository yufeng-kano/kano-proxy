//! HTTP route groups. `core_routes` assembles them in the order
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
/// `shadowed` lists core paths the edition serves itself (docs/rust-server.md § Module map);
/// the core skips mounting those groups so `Router::merge` never sees a duplicate. Only
/// `/api/changelog` is shadowable today (the hosted edition answers it as unavailable).
pub fn core_routes(state: &AppState, shadowed: &[String]) -> Router<AppState> {
    let shadows = |path: &str| shadowed.iter().any(|s| s == path);
    let admin = Router::new()
        .nest("/api/auth", auth::routes())
        .nest("/api/keys", keys::routes())
        .nest("/api/cli", cli::routes())
        .nest("/api/providers", providers::routes())
        .nest("/api/custom-providers", custom_providers::routes())
        .nest("/api/models", models::routes())
        .nest("/api/model-groups", model_groups::routes())
        .nest("/api/usage", usage::routes())
        .nest("/api/logs", logs::routes())
        .merge(if shadows("/api/changelog") { Router::new() } else { Router::new().nest("/api/changelog", changelog::routes()) })
        .layer(crate::http::cors::admin_cors(&state.config().app_url))
        .layer(crate::http::cors::no_store_layer());
    // `/agent/v1` is its own namespace beside `/api/*`: token-authenticated, never
    // session, and no CORS — the CLI is the only caller (docs/cli.md § Server routes).
    // The LLM surfaces (`/openai/v1`, `/anthropic`, `/g`) carry permissive, never credentialed
    // CORS and the API-key middleware, which also runs the edition's `RequestPolicy`
    // (`openai::llm_routes`, matching `application.ts`).
    Router::new().merge(admin).nest("/agent/v1", agent::routes()).merge(openai::llm_routes(state))
}
