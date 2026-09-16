//! Anthropic-surface protocol details for
//! dispatch (docs/api.md § In-stream errors, § Errors).

use bytes::Bytes;
use serde_json::{json, Value};

use crate::logging::usage_capture::{
    create_anthropic_sse_usage_sniffer, from_anthropic_usage, NormalizedUsage, UsageSniffer,
};
use crate::proxy::wire::{frame_bytes, NonStreamResponse, Wire};

/// Exact text from docs/api.md § Keepalive and idle timeout.
pub const ANTHROPIC_STALL_FRAME: &str = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"upstream stalled: no data received for 120s\"}}\n\n";

fn frame(message: &str, error_type: &str) -> Bytes {
    let payload = json!({ "type": "error", "error": { "type": error_type, "message": message } });
    frame_bytes(format!("event: error\ndata: {payload}\n\n"))
}

fn error_type_from_body(text: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(t) = value.get("error").and_then(|e| e.get("type")).and_then(Value::as_str) {
            if !t.is_empty() {
                return t.to_string();
            }
        }
        if value.get("type").and_then(Value::as_str) == Some("error")
            && value.get("error").map(Value::is_object).unwrap_or(false)
        {
            return "api_error".to_string();
        }
    }
    "api_error".to_string()
}

/// `anthropicWire`.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnthropicWire;

pub const ANTHROPIC_WIRE: AnthropicWire = AnthropicWire;

impl Wire for AnthropicWire {
    fn stall_frame(&self) -> Bytes {
        Bytes::from_static(ANTHROPIC_STALL_FRAME.as_bytes())
    }
    fn no_account_frame(&self, provider: &str) -> Bytes {
        frame(&format!("No usable {provider} account"), "invalid_request_error")
    }
    fn unavailable_frame(&self) -> Bytes {
        frame("upstream_unavailable", "api_error")
    }
    fn upstream_error_frame(&self) -> Bytes {
        frame("upstream error", "api_error")
    }
    fn request_too_large_frame(&self, message: &str) -> Bytes {
        frame(message, "invalid_request_error")
    }
    fn upstream_error_frame_from_body(&self, message: &str, text: &str) -> Bytes {
        frame(message, &error_type_from_body(text))
    }

    fn no_account_body(&self, provider: &str) -> Value {
        json!({ "type": "error", "error": { "type": "invalid_request_error", "message": format!("No usable {provider} account") } })
    }
    fn unavailable_body(&self) -> Value {
        json!({ "type": "error", "error": { "type": "api_error", "message": "upstream_unavailable" } })
    }
    fn upstream_error_body(&self) -> Value {
        json!({ "type": "error", "error": { "type": "api_error", "message": "upstream error" } })
    }
    fn request_too_large_body(&self, message: &str) -> Value {
        json!({ "type": "error", "error": { "type": "invalid_request_error", "message": message } })
    }

    fn non_stream_response(&self) -> NonStreamResponse {
        NonStreamResponse::AsReceived
    }

    fn create_usage_sniffer(&self) -> Box<dyn UsageSniffer> {
        create_anthropic_sse_usage_sniffer()
    }
    fn parse_usage(&self, usage: Option<&Value>) -> NormalizedUsage {
        from_anthropic_usage(usage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: Bytes) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn stall_frame_is_byte_identical_to_the_documented_text() {
        assert_eq!(
            text(ANTHROPIC_WIRE.stall_frame()),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"upstream stalled: no data received for 120s\"}}\n\n"
        );
    }

    #[test]
    fn eager_commit_frames() {
        assert_eq!(
            text(ANTHROPIC_WIRE.no_account_frame("claude-code")),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"No usable claude-code account\"}}\n\n"
        );
        assert_eq!(
            text(ANTHROPIC_WIRE.unavailable_frame()),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"upstream_unavailable\"}}\n\n"
        );
        assert_eq!(
            text(ANTHROPIC_WIRE.upstream_error_frame()),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"upstream error\"}}\n\n"
        );
        assert_eq!(
            text(ANTHROPIC_WIRE.request_too_large_frame("big")),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"big\"}}\n\n"
        );
    }

    #[test]
    fn the_upstream_error_type_is_read_out_of_the_upstream_body() {
        assert!(text(ANTHROPIC_WIRE.upstream_error_frame_from_body("nope", r#"{"error":{"type":"overloaded_error"}}"#))
            .contains("\"type\":\"overloaded_error\""));
        for body in ["<html>", r#"{"type":"error","error":{}}"#, "{}"] {
            assert!(text(ANTHROPIC_WIRE.upstream_error_frame_from_body("nope", body)).contains("\"type\":\"api_error\""));
        }
    }

    #[test]
    fn non_stream_bodies() {
        assert_eq!(
            ANTHROPIC_WIRE.no_account_body("claude-code").to_string(),
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"No usable claude-code account"}}"#
        );
        assert_eq!(
            ANTHROPIC_WIRE.unavailable_body().to_string(),
            r#"{"type":"error","error":{"type":"api_error","message":"upstream_unavailable"}}"#
        );
        assert_eq!(
            ANTHROPIC_WIRE.upstream_error_body().to_string(),
            r#"{"type":"error","error":{"type":"api_error","message":"upstream error"}}"#
        );
        assert_eq!(
            ANTHROPIC_WIRE.request_too_large_body("big").to_string(),
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"big"}}"#
        );
        assert_eq!(ANTHROPIC_WIRE.non_stream_response(), NonStreamResponse::AsReceived);
    }

    #[test]
    fn parse_usage_uses_the_anthropic_normalizer() {
        let usage = serde_json::json!({"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 2});
        assert_eq!(ANTHROPIC_WIRE.parse_usage(Some(&usage)).prompt_tokens, Some(12));
        assert_eq!(ANTHROPIC_WIRE.parse_usage(None), crate::logging::usage_capture::NULL_USAGE);
    }
}
