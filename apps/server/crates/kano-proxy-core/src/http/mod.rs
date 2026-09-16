//! HTTP envelope helpers shared by every route group (docs/api.md § Errors).

pub mod cors;
pub mod errors;

pub use errors::{ApiError, Surface};
