//! kano-proxy server core (docs/rust-server.md).
//!
//! Editions compose the core through [`app::build_router`] with [`extensions::Extensions`]:
//! a request policy, extra routes and a pool extension, passed at composition time so two
//! routers never leak into each other. The documentation under `docs/` is the contract for
//! every module here (docs/rust-server.md § Module map).

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
