//! HTTP envelope helpers shared by every route group (docs/api.md § Errors).

pub mod errors;

pub use errors::{ApiError, Surface};
