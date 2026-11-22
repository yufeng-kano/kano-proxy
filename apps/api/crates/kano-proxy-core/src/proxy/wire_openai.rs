//! OpenAI-surface protocol details for dispatch
//! (docs/api.md § In-stream errors, § Errors).

use bytes::Bytes;
use serde_json::{json, Value};

use crate::logging::usage_capture::{create_openai_sse_usage_sniffer, from_openai_usage, NormalizedUsage, UsageSniffer};
use crate::proxy::wire::{error_type_from_body, NonStreamResponse, Wire};

/// Exact text from docs/api.md § Keepalive and idle timeout.
const OPENAI_STALL_FRAME: &str = "data: {\"error\":{\"message\":\"upstream stalled: no data received for 120s\",\"type\":\"api_error\",\"code\":\"upstream_stall\"}}\n\n";

fn frame(message: &str, error_type: &str, code: &str) -> Bytes {
    let payload = json!({ "error": { "message": message, "type": error_type, "code": code } });
    Bytes::from(format!("data: {payload}\n\n"))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAiWire;

impl Wire for OpenAiWire {
    fn stall_frame(&self) -> Bytes {
        Bytes::from_static(OPENAI_STALL_FRAME.as_bytes())
    }
    fn no_account_frame(&self, provider: &str) -> Bytes {
        frame(&format!("No usable {provider} account for this user"), "invalid_request_error", "no_upstream_account")
    }
    fn unavailable_frame(&self) -> Bytes {
        frame("All upstream accounts unavailable", "api_error", "upstream_unavailable")
    }
    fn upstream_error_frame(&self) -> Bytes {
        frame("upstream error", "api_error", "upstream_error")
    }
    fn upstream_error_frame_from_body(&self, message: &str, text: &str) -> Bytes {
        frame(message, &error_type_from_body(text), "upstream_error")
    }

    fn no_account_body(&self, provider: &str) -> Value {
        json!({ "error": {
            "message": format!("No usable {provider} account for this user"),
            "type": "invalid_request_error",
            "code": "no_upstream_account",
        }})
    }
    fn unavailable_body(&self) -> Value {
        json!({ "error": { "message": "All upstream accounts unavailable", "code": "upstream_unavailable" } })
    }
    fn upstream_error_body(&self) -> Value {
        json!({ "error": { "message": "upstream error", "code": "upstream_error" } })
    }

    fn non_stream_response(&self) -> NonStreamResponse {
        NonStreamResponse::ContentTypeOnly
    }

    fn create_usage_sniffer(&self) -> Box<dyn UsageSniffer> {
        create_openai_sse_usage_sniffer()
    }
    fn parse_usage(&self, usage: Option<&Value>) -> NormalizedUsage {
        from_openai_usage(usage)
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
            text(OpenAiWire.no_account_frame("grok")),
            "data: {\"error\":{\"message\":\"No usable grok account for this user\",\"type\":\"invalid_request_error\",\"code\":\"no_upstream_account\"}}\n\n"
        );
        assert_eq!(
            text(OpenAiWire.unavailable_frame()),
            "data: {\"error\":{\"message\":\"All upstream accounts unavailable\",\"type\":\"api_error\",\"code\":\"upstream_unavailable\"}}\n\n"
        );
        assert_eq!(
            text(OpenAiWire.upstream_error_frame()),
            "data: {\"error\":{\"message\":\"upstream error\",\"type\":\"api_error\",\"code\":\"upstream_error\"}}\n\n"
        );
    }

    #[test]
    fn the_upstream_error_type_is_read_out_of_the_upstream_body() {
        assert_eq!(
            text(OpenAiWire.upstream_error_frame_from_body("nope", r#"{"error":{"type":"rate_limit_error"}}"#)),
            "data: {\"error\":{\"message\":\"nope\",\"type\":\"rate_limit_error\",\"code\":\"upstream_error\"}}\n\n"
        );
        // Not JSON, an empty type, or no type at all all fall back to api_error.
        for body in ["<html>", r#"{"error":{"type":""}}"#, "{}"] {
            assert!(text(OpenAiWire.upstream_error_frame_from_body("nope", body)).contains("\"type\":\"api_error\""));
        }
    }

    #[test]
    fn non_stream_bodies() {
        assert_eq!(
            OpenAiWire.no_account_body("codex"),
            serde_json::json!({"error":{"message":"No usable codex account for this user","type":"invalid_request_error","code":"no_upstream_account"}})
        );
        assert_eq!(
            OpenAiWire.unavailable_body().to_string(),
            r#"{"error":{"message":"All upstream accounts unavailable","code":"upstream_unavailable"}}"#
        );
        assert_eq!(
            OpenAiWire.upstream_error_body().to_string(),
            r#"{"error":{"message":"upstream error","code":"upstream_error"}}"#
        );
        assert_eq!(OpenAiWire.non_stream_response(), NonStreamResponse::ContentTypeOnly);
    }

    #[test]
    fn parse_usage_uses_the_openai_normalizer() {
        let usage = serde_json::json!({"prompt_tokens": 9, "completion_tokens": 3});
        assert_eq!(OpenAiWire.parse_usage(Some(&usage)).prompt_tokens, Some(9));
        assert_eq!(OpenAiWire.parse_usage(None), crate::logging::usage_capture::NULL_USAGE);
    }
}
