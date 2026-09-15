//! kano-proxy server core (docs/rust-server.md).
//!
//! Editions compose the core through [`app::build_router`] with [`extensions::Extensions`],
//! mirroring the TypeScript `createApplication({requestPolicy, registerRoutes, poolExtension})`
//! entry point. Everything an edition needs is re-exported here; internal modules are not a
//! stable surface.

pub mod app;
pub mod config;
pub mod crypto;
pub mod db;
pub mod extensions;
pub mod ids;

pub use app::{build_router, serve, AppState};
pub use config::CoreConfig;
pub use extensions::Extensions;
