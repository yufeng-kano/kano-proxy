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
pub fn core_routes(_state: &AppState) -> Router<AppState> {
    Router::new()
}
