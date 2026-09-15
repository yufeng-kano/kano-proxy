//! kano-proxy server core (docs/rust-server.md).
//!
//! Editions compose the core through [`app::build_router`] with [`extensions::Extensions`],
//! mirroring the TypeScript `createApplication({requestPolicy, registerRoutes, poolExtension})`
//! entry point. Module names follow `apps/api/src` one to one (docs/rust-server.md § Module
//! map) so the TypeScript file and its docs remain the reference for each port.

pub mod app;
pub mod auth;
pub mod cache;
pub mod catalog;
pub mod changelog;
pub mod config;
pub mod crypto;
pub mod db;
pub mod extensions;
pub mod http;
pub mod ids;
pub mod logging;
pub mod maintenance;
pub mod pool;
pub mod pricing;
pub mod providers;
pub mod proxy;
pub mod routes;
pub mod routing;
pub mod tunnel;
pub mod upstream;
pub mod utils;

pub use app::{build_router, serve, AppState};
pub use config::CoreConfig;
pub use extensions::Extensions;
