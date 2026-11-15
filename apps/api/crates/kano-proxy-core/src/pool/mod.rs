//! Credential persistence, benching and the pool extension contract.

pub mod acquire;
pub mod bench;
pub mod extension;
pub mod sign_in;

pub use acquire::{AcquiredAccount, StoredCredential};
pub use extension::{AttemptLease, ListSharedOptions, PoolExtension, ReserveOutcome, SharedAccount, ShareInfo};
