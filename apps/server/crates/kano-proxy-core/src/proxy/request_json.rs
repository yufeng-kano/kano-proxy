//! Reading and re-reading a request body as JSON.
//!
//! The TypeScript helper exists to keep the parsed proxy body reusable without retaining a
//! second, potentially huge raw-text copy in Hono's body cache. Axum has no such cache: the
//! body is read once as [`Bytes`] and parsed once here, and the parsed [`Map`] is what every
//! later stage (dispatch, adapters, converters) passes around — the raw bytes are dropped with
//! the caller's local.
//!
//! JSON is `serde_json::Value` with `preserve_order`, so client key order survives into the
//! bodies the adapters build (docs/rust-server.md § Module map).

use bytes::Bytes;
use serde_json::{Map, Value};

/// Why a proxy body could not be used. The TypeScript `request.json()` rejects on the first
/// and silently yields a non-object for the second; both are client errors here.
#[derive(Debug, thiserror::Error)]
pub enum ProxyJsonError {
    #[error("invalid JSON body")]
    NotJson(#[from] serde_json::Error),
    #[error("request body must be a JSON object")]
    NotObject,
}

/// `readProxyJson(request)`: parse the request body into the object every proxy route works
/// with. Called once per request; the result is passed on rather than re-parsed.
pub fn read_proxy_json(body: &Bytes) -> Result<Map<String, Value>, ProxyJsonError> {
    match serde_json::from_slice::<Value>(body)? {
        Value::Object(map) => Ok(map),
        _ => Err(ProxyJsonError::NotObject),
    }
}

impl ProxyJsonError {
    /// The `400 invalid_request_error` envelope for the client's surface (docs/api.md § Errors).
    pub fn into_api_error(self, surface: crate::http::errors::Surface) -> crate::http::errors::ApiError {
        use crate::http::errors::{ApiError, Surface};
        let message = self.to_string();
        match surface {
            Surface::OpenAI => ApiError::openai(
                axum::http::StatusCode::BAD_REQUEST,
                &message,
                "invalid_request_error",
                "invalid_request",
            ),
            Surface::Anthropic => {
                ApiError::anthropic(axum::http::StatusCode::BAD_REQUEST, "invalid_request_error", &message)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_an_object_body_once_and_keeps_client_key_order() {
        let body = Bytes::from_static(br#"{"model":"m","messages":[],"stream":true}"#);
        let parsed = read_proxy_json(&body).unwrap();
        assert_eq!(parsed.keys().collect::<Vec<_>>(), vec!["model", "messages", "stream"]);
        assert_eq!(parsed.get("model"), Some(&json!("m")));
        assert_eq!(Value::Object(parsed).to_string(), r#"{"model":"m","messages":[],"stream":true}"#);
    }

    #[test]
    fn rejects_malformed_json_and_non_objects() {
        assert!(matches!(read_proxy_json(&Bytes::from_static(b"{")), Err(ProxyJsonError::NotJson(_))));
        assert!(matches!(read_proxy_json(&Bytes::from_static(b"[1,2]")), Err(ProxyJsonError::NotObject)));
        assert!(matches!(read_proxy_json(&Bytes::from_static(b"null")), Err(ProxyJsonError::NotObject)));
    }

    #[test]
    fn a_large_body_parses_without_a_second_text_copy() {
        let big = format!(r#"{{"input":"{}"}}"#, "x".repeat(1024 * 1024));
        let parsed = read_proxy_json(&Bytes::from(big)).unwrap();
        assert_eq!(parsed.get("input").and_then(Value::as_str).map(str::len), Some(1024 * 1024));
    }

    #[test]
    fn surface_specific_error_envelopes() {
        use crate::http::errors::Surface;
        let err = read_proxy_json(&Bytes::from_static(b"[]")).unwrap_err();
        let body = err.into_api_error(Surface::Anthropic).body();
        assert_eq!(body["type"], json!("error"));
        assert_eq!(body["error"]["type"], json!("invalid_request_error"));
    }
}
