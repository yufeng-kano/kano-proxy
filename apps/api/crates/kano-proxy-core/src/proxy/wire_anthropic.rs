//! Anthropic-surface protocol details for
//! dispatch (docs/api.md § In-stream errors, § Errors).

use bytes::Bytes;
use serde_json::{json, Value};

use crate::logging::usage_capture::{
    create_anthropic_sse_usage_sniffer, from_anthropic_usage, NormalizedUsage, UsageSniffer,
};
use crate::proxy::wire::{error_type_from_body, NonStreamResponse, Wire};

/// Exact text from docs/api.md § Keepalive and idle timeout.
const ANTHROPIC_STALL_FRAME: &str = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"upstream stalled: no data received for 120s\"}}\n\n";

fn frame(message: &str, error_type: &str) -> Bytes {
    let payload = json!({ "type": "error", "error": { "type": error_type, "message": message } });
    Bytes::from(format!("event: error\ndata: {payload}\n\n"))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AnthropicWire;

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
    fn eager_commit_frames() {
        assert_eq!(
            text(AnthropicWire.no_account_frame("claude-code")),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"No usable claude-code account\"}}\n\n"
        );
        assert_eq!(
            text(AnthropicWire.unavailable_frame()),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"upstream_unavailable\"}}\n\n"
        );
        assert_eq!(
            text(AnthropicWire.upstream_error_frame()),
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"upstream error\"}}\n\n"
        );
    }

    #[test]
    fn the_upstream_error_type_is_read_out_of_the_upstream_body() {
        assert!(text(AnthropicWire.upstream_error_frame_from_body("nope", r#"{"error":{"type":"overloaded_error"}}"#))
            .contains("\"type\":\"overloaded_error\""));
        for body in ["<html>", r#"{"type":"error","error":{}}"#, "{}"] {
            assert!(text(AnthropicWire.upstream_error_frame_from_body("nope", body)).contains("\"type\":\"api_error\""));
        }
    }

    #[test]
    fn non_stream_bodies() {
        assert_eq!(
            AnthropicWire.no_account_body("claude-code").to_string(),
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"No usable claude-code account"}}"#
        );
        assert_eq!(
            AnthropicWire.unavailable_body().to_string(),
            r#"{"type":"error","error":{"type":"api_error","message":"upstream_unavailable"}}"#
        );
        assert_eq!(
            AnthropicWire.upstream_error_body().to_string(),
            r#"{"type":"error","error":{"type":"api_error","message":"upstream error"}}"#
        );
        assert_eq!(AnthropicWire.non_stream_response(), NonStreamResponse::AsReceived);
    }

    #[test]
    fn parse_usage_uses_the_anthropic_normalizer() {
        let usage = serde_json::json!({"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 2});
        assert_eq!(AnthropicWire.parse_usage(Some(&usage)).prompt_tokens, Some(12));
        assert_eq!(AnthropicWire.parse_usage(None), crate::logging::usage_capture::NULL_USAGE);
    }
}
