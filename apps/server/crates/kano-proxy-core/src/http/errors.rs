//! JSON error envelopes for both client surfaces (docs/api.md § Errors):
//! OpenAI `{"error":{"message","type","code"}}` and Anthropic
//! `{"type":"error","error":{"type","message"}}`. Auth failures pick the surface by
//! path prefix (`/anthropic` → Anthropic, otherwise OpenAI).

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    OpenAI,
    Anthropic,
}

impl Surface {
    /// The surface for a request path: `/anthropic/...` and `/g/<slug>/anthropic/...` are
    /// Anthropic; everything else answers in the OpenAI shape.
    pub fn from_path(path: &str) -> Surface {
        let rest = path.strip_prefix("/g/").and_then(|r| r.split_once('/').map(|(_, rest)| rest)).unwrap_or(path.trim_start_matches('/'));
        if rest == "anthropic" || rest.starts_with("anthropic/") {
            Surface::Anthropic
        } else {
            Surface::OpenAI
        }
    }

    pub fn envelope(self, message: &str, error_type: &str, code: Option<&str>) -> Value {
        match self {
            Surface::OpenAI => {
                let mut e = json!({ "message": message, "type": error_type });
                if let Some(code) = code {
                    e["code"] = Value::String(code.to_string());
                }
                json!({ "error": e })
            }
            Surface::Anthropic => json!({ "type": "error", "error": { "type": error_type, "message": message } }),
        }
    }
}

/// A JSON error response. `code` is the OpenAI-surface `error.code` (also the
/// `request_logs.error_code` vocabulary where one applies); `should_retry` sets the
/// `x-should-retry` header when present; `retry_after` sets `Retry-After` seconds.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub surface: Surface,
    pub error_type: String,
    pub message: String,
    pub code: Option<String>,
    pub should_retry: Option<bool>,
    pub retry_after: Option<u64>,
    pub headers: Vec<(header::HeaderName, HeaderValue)>,
}

impl ApiError {
    pub fn new(status: StatusCode, surface: Surface, error_type: &str, message: &str) -> Self {
        Self {
            status,
            surface,
            error_type: error_type.to_string(),
            message: message.to_string(),
            code: None,
            should_retry: None,
            retry_after: None,
            headers: Vec::new(),
        }
    }
    pub fn openai(status: StatusCode, message: &str, error_type: &str, code: &str) -> Self {
        Self::new(status, Surface::OpenAI, error_type, message).code(code)
    }
    pub fn anthropic(status: StatusCode, error_type: &str, message: &str) -> Self {
        Self::new(status, Surface::Anthropic, error_type, message)
    }
    pub fn code(mut self, code: &str) -> Self {
        self.code = Some(code.to_string());
        self
    }
    pub fn should_retry(mut self, retry: bool) -> Self {
        self.should_retry = Some(retry);
        self
    }
    pub fn retry_after(mut self, seconds: u64) -> Self {
        self.retry_after = Some(seconds);
        self
    }
    pub fn header(mut self, name: header::HeaderName, value: HeaderValue) -> Self {
        self.headers.push((name, value));
        self
    }
    /// Admin `/api/*` errors: OpenAI-shaped `{"error":{"message","type","code"}}`.
    pub fn admin(status: StatusCode, code: &str, message: &str) -> Self {
        Self::openai(status, message, admin_type(status), code)
    }
    pub fn unauthorized(surface: Surface) -> Self {
        match surface {
            Surface::OpenAI => Self::openai(StatusCode::UNAUTHORIZED, "Unauthorized", "authentication_error", "unauthorized"),
            Surface::Anthropic => Self::anthropic(StatusCode::UNAUTHORIZED, "authentication_error", "Unauthorized"),
        }
    }
    pub fn body(&self) -> Value {
        self.surface.envelope(&self.message, &self.error_type, self.code.as_deref())
    }
}

fn admin_type(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 | 404 | 409 | 422 => "invalid_request_error",
        401 | 403 => "authentication_error",
        429 => "rate_limit_error",
        _ => "api_error",
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Some(retry) = self.should_retry {
            headers.insert("x-should-retry", HeaderValue::from_static(if retry { "true" } else { "false" }));
        }
        if let Some(seconds) = self.retry_after {
            headers.insert(header::RETRY_AFTER, HeaderValue::from_str(&seconds.to_string()).expect("digits"));
        }
        for (name, value) in &self.headers {
            headers.insert(name.clone(), value.clone());
        }
        (self.status, headers, self.body().to_string()).into_response()
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status, self.error_type, self.message)
    }
}

impl std::error::Error for ApiError {}

/// Storage or other infrastructure failure inside a route: `500 internal_error`. Route
/// handlers return `Result<_, ApiError>`, so `?` on a `sqlx::Error` works.
impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        tracing::error!(error = %err, "storage error");
        Self::admin(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "internal error")
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        tracing::error!(error = %err, "internal error");
        Self::admin(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "internal error")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_by_path() {
        assert_eq!(Surface::from_path("/anthropic/v1/messages"), Surface::Anthropic);
        assert_eq!(Surface::from_path("/g/team/anthropic/v1/models"), Surface::Anthropic);
        assert_eq!(Surface::from_path("/openai/v1/chat/completions"), Surface::OpenAI);
        assert_eq!(Surface::from_path("/g/x/openai/v1/models"), Surface::OpenAI);
        assert_eq!(Surface::from_path("/api/keys"), Surface::OpenAI);
    }

    #[test]
    fn envelopes() {
        let o = ApiError::openai(StatusCode::BAD_REQUEST, "bad", "invalid_request_error", "invalid_model").body();
        assert_eq!(o, json!({"error":{"message":"bad","type":"invalid_request_error","code":"invalid_model"}}));
        let a = ApiError::anthropic(StatusCode::UNAUTHORIZED, "authentication_error", "Unauthorized").body();
        assert_eq!(a, json!({"type":"error","error":{"type":"authentication_error","message":"Unauthorized"}}));
    }
}
