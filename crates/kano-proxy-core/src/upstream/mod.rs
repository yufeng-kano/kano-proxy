//! Outbound HTTP to upstream providers. Every adapter sends through
//! [`UpstreamTransport`] so tests substitute [`MockTransport`] instead of a real network,
//! as the TypeScript suite stubs `fetch` (docs/testing.md; no paid upstream traffic).

pub mod transport;

pub use transport::{MockTransport, ReqwestTransport, UpstreamRequest, UpstreamResponse, UpstreamTransport};
