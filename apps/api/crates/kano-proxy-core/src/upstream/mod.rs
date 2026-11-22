//! Outbound HTTP to upstream providers. Every adapter sends through
//! [`UpstreamTransport`] so tests substitute [`MockTransport`] instead of a real network
//! (docs/testing.md; no paid upstream traffic).

pub mod transport;

pub use transport::{ByteStream, MockTransport, ReqwestTransport, TransportError, UpstreamRequest, UpstreamResponse, UpstreamTransport};
