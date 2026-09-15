//! Port of apps/api/src/proxy/wire.ts (docs/api.md § In-stream errors, § Errors).
//!
//! Everything about a dispatch surface that depends on the client protocol. The transports in
//! `dispatch` call these and never branch on OpenAI vs Anthropic themselves; the two
//! implementations are [`crate::proxy::wire_openai`] and [`crate::proxy::wire_anthropic`].

use bytes::Bytes;
use serde_json::Value;

use crate::logging::usage_capture::{NormalizedUsage, UsageSniffer};

/// How a non-stream upstream response reaches the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonStreamResponse {
    /// OpenAI surface: read the body and rebuild the response with just `content-type`, as the
    /// Chat Completions path always has.
    ContentTypeOnly,
    /// Anthropic surface: return the upstream response untouched, every header included
    /// (docs/api.md § Eager streaming commit).
    AsReceived,
}

pub trait Wire: Send + Sync {
    /// Terminal frame emitted after 120s of upstream silence while piping (docs/api.md
    /// § Keepalive and idle timeout). Byte-identical to the documented text.
    fn stall_frame(&self) -> Bytes;

    /// Eager-commit terminal error frames (docs/api.md § In-stream errors).
    fn no_account_frame(&self, provider: &str) -> Bytes;
    fn unavailable_frame(&self) -> Bytes;
    fn upstream_error_frame(&self) -> Bytes;
    fn request_too_large_frame(&self, message: &str) -> Bytes;
    /// Upstream non-2xx after failover: `message` is already extracted from `text`, the error
    /// type is read from `text` in the surface's own envelope shape.
    fn upstream_error_frame_from_body(&self, message: &str, text: &str) -> Bytes;

    /// Non-stream JSON error envelopes (docs/api.md § Errors).
    fn no_account_body(&self, provider: &str) -> Value;
    fn unavailable_body(&self) -> Value;
    fn upstream_error_body(&self) -> Value;
    fn request_too_large_body(&self, message: &str) -> Value;

    fn non_stream_response(&self) -> NonStreamResponse;

    /// Token usage from a piped SSE body (docs/logging.md § Token usage capture).
    fn create_usage_sniffer(&self) -> Box<dyn UsageSniffer>;
    /// Token usage from a non-stream JSON body's `usage`.
    fn parse_usage(&self, usage: Option<&Value>) -> NormalizedUsage;
}

/// Shared by both implementations: an SSE frame is a complete event, terminated by a blank line.
pub(crate) fn frame_bytes(text: String) -> Bytes {
    Bytes::from(text.into_bytes())
}
