//! Reading a request body as JSON.
//!
//! The body is read once as [`Bytes`] and parsed once here, and the parsed [`Map`] is what every
//! later stage (dispatch, adapters, converters) passes around — the raw bytes are dropped with
//! the caller's local.
//!
//! JSON is `serde_json::Value` with `preserve_order`, so client key order survives into the
//! bodies the adapters build (docs/rust-server.md § Module map).

use bytes::Bytes;
use serde_json::{Map, Value};

/// The JSON object every proxy route works with; `None` for a body that is not JSON or not an
/// object, both of which the routes answer as a client error. Called once per request; the
/// result is passed on rather than re-parsed.
pub fn read_proxy_json(body: &Bytes) -> Option<Map<String, Value>> {
    match serde_json::from_slice::<Value>(body).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
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
        for body in [&b"{"[..], b"[1,2]", b"null"] {
            assert!(read_proxy_json(&Bytes::copy_from_slice(body)).is_none());
        }
    }
}
