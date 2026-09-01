//! Agent tunnel wire protocol v1, CLI side (docs/cli.md § Wire protocol).
//! Mirrors apps/api/src/do/protocol.ts: JSON text control frames, binary body
//! frames `[u32 BE request id][u8 kind][chunk]`. Both ends enforce the bounds.

use serde::{Deserialize, Serialize};

pub const AGENT_PROTO: u32 = 1;
pub const BODY_KIND_REQUEST: u8 = 0;
pub const BODY_KIND_RESPONSE: u8 = 1;
pub const MAX_CHUNK_BYTES: usize = 1024 * 1024;
pub const CLOSE_REPLACED: u16 = 4001;
pub const CLOSE_TOKEN_EXPIRED: u16 = 4003;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum ControlFrame {
    #[serde(rename = "hello")]
    Hello { proto: u32, slug: String },
    #[serde(rename = "req")]
    Req {
        id: u32,
        method: String,
        path: String,
        #[serde(default)]
        headers: std::collections::BTreeMap<String, String>,
    },
    #[serde(rename = "req_end")]
    ReqEnd { id: u32 },
    #[serde(rename = "res")]
    Res {
        id: u32,
        status: u16,
        headers: std::collections::BTreeMap<String, String>,
    },
    #[serde(rename = "res_end")]
    ResEnd { id: u32 },
    #[serde(rename = "res_err")]
    ResErr { id: u32, reason: String },
    #[serde(rename = "models")]
    Models { models: Vec<String> },
    #[serde(rename = "cancel")]
    Cancel { id: u32 },
}

pub fn parse_control(text: &str) -> Option<ControlFrame> {
    serde_json::from_str(text).ok()
}

pub fn encode_control(frame: &ControlFrame) -> String {
    serde_json::to_string(frame).expect("control frames are always serializable")
}

pub fn encode_binary(id: u32, kind: u8, chunk: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + chunk.len());
    out.extend_from_slice(&id.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(chunk);
    out
}

pub fn decode_binary(data: &[u8]) -> Option<(u32, u8, &[u8])> {
    if data.len() < 5 {
        return None;
    }
    let id = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    Some((id, data[4], &data[5..]))
}

/// The CLI's own copy of the path allowlist — it only ever joins these
/// suffixes onto its one configured target base, so it is structurally not a
/// general proxy even against a compromised server.
pub fn is_allowed_path(format: &str, path: &str) -> bool {
    match format {
        "openai" => matches!(path, "/chat/completions" | "/models" | "/audio/transcriptions"),
        "anthropic" => matches!(path, "/v1/messages" | "/v1/messages/count_tokens" | "/v1/models"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_roundtrip() {
        let frame = encode_binary(70000, BODY_KIND_RESPONSE, b"hello");
        let (id, kind, chunk) = decode_binary(&frame).unwrap();
        assert_eq!(id, 70000);
        assert_eq!(kind, BODY_KIND_RESPONSE);
        assert_eq!(chunk, b"hello");
        assert!(decode_binary(&[0, 0, 1]).is_none());
    }

    #[test]
    fn control_roundtrip() {
        let parsed = parse_control(r#"{"t":"req","id":3,"method":"POST","path":"/chat/completions","headers":{"content-type":"application/json"}}"#).unwrap();
        match parsed {
            ControlFrame::Req { id, method, path, headers } => {
                assert_eq!(id, 3);
                assert_eq!(method, "POST");
                assert_eq!(path, "/chat/completions");
                assert_eq!(headers.get("content-type").unwrap(), "application/json");
            }
            other => panic!("wrong frame: {other:?}"),
        }
        let hello = parse_control(r#"{"t":"hello","proto":1,"slug":"my-mac"}"#).unwrap();
        assert!(matches!(hello, ControlFrame::Hello { proto: 1, .. }));
        assert!(parse_control("junk").is_none());
    }

    #[test]
    fn res_encodes_with_tag() {
        let s = encode_control(&ControlFrame::Res {
            id: 1,
            status: 200,
            headers: [("content-type".to_string(), "text/event-stream".to_string())]
                .into_iter()
                .collect(),
        });
        assert!(s.contains(r#""t":"res""#));
        assert!(s.contains(r#""status":200"#));
    }

    #[test]
    fn allowlist_by_format() {
        assert!(is_allowed_path("openai", "/chat/completions"));
        assert!(is_allowed_path("openai", "/audio/transcriptions"));
        assert!(!is_allowed_path("openai", "/v1/messages"));
        assert!(is_allowed_path("anthropic", "/v1/messages/count_tokens"));
        assert!(!is_allowed_path("anthropic", "/chat/completions"));
        assert!(!is_allowed_path("openai", "/admin"));
    }
}
