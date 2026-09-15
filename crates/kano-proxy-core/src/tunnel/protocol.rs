//! Port of apps/api/src/do/protocol.ts — agent tunnel wire protocol v1
//! (docs/cli.md § Wire protocol). Control frames are JSON text, body bytes are
//! binary frames `[u32 BE request id][u8 kind][chunk]`. The frame enum is the
//! mirror image of the CLI's `apps/cli/src/protocol.rs`, so released CLI
//! binaries keep talking to this server unchanged. Both ends enforce the
//! byte/path bounds — this module is the server half.

use serde::{Deserialize, Serialize};

pub const AGENT_PROTO: u32 = 1;

/// Kind 0 = request body (server → CLI), kind 1 = response body (CLI → server).
pub const BODY_KIND_REQUEST: u8 = 0;
pub const BODY_KIND_RESPONSE: u8 = 1;

/// Small frames keep memory flat and interleave fairly across multiplexed requests.
pub const MAX_CHUNK_BYTES: usize = 1024 * 1024;
/// Excess is refused with fault `busy` so group failover takes the next target instead of queueing.
pub const MAX_INFLIGHT: usize = 4;
/// Per-request response buffer — an honest bound in lieu of credit-based flow control.
pub const RESPONSE_BUFFER_LIMIT_BYTES: usize = 8 * 1024 * 1024;
/// Per-request request-body ceiling; the mirror of the response-side buffer.
pub const REQUEST_BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;
/// First `res` frame must arrive within this of `req_end`.
pub const FIRST_RES_TIMEOUT_MS: u64 = 120_000;

pub const CLOSE_REPLACED: u16 = 4001;
pub const CLOSE_TOKEN_EXPIRED: u16 = 4003;
/// Retryable server-side failure (e.g. a models-report write). The Worker was limited to
/// 1000/3000-4999 close codes; the code stays 4008 because released CLIs treat it as retryable.
pub const CLOSE_RETRY: u16 = 4008;

/// Agent-reported catalog bounds mirror the custom manual-list rules (docs/cli.md § Model catalog).
pub const MAX_REPORTED_MODELS: usize = 100;
pub const MAX_REPORTED_MODEL_ID_LENGTH: usize = 128;

const OPENAI_PATHS: [&str; 3] = ["/chat/completions", "/models", "/audio/transcriptions"];
const ANTHROPIC_PATHS: [&str; 3] = ["/v1/messages", "/v1/messages/count_tokens", "/v1/models"];

/// `cli_providers.format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliProviderFormat {
    OpenAI,
    Anthropic,
}

impl CliProviderFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            CliProviderFormat::OpenAI => "openai",
            CliProviderFormat::Anthropic => "anthropic",
        }
    }
    /// `null` for anything but the two known formats, as the Worker's header check was.
    pub fn parse(value: &str) -> Option<CliProviderFormat> {
        match value {
            "openai" => Some(CliProviderFormat::OpenAI),
            "anthropic" => Some(CliProviderFormat::Anthropic),
            _ => None,
        }
    }
}

impl std::fmt::Display for CliProviderFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Path allowlist by provider format (docs/cli.md § Wire protocol § Bounds).
pub fn is_allowed_path(format: CliProviderFormat, path: &str) -> bool {
    match format {
        CliProviderFormat::OpenAI => OPENAI_PATHS.contains(&path),
        CliProviderFormat::Anthropic => ANTHROPIC_PATHS.contains(&path),
    }
}

/// The tri-state guard's fault vocabulary (docs/cli.md § Failover semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFaultReason {
    Offline,
    Busy,
    Timeout,
    Replaced,
    TooLarge,
    Protocol,
}

impl AgentFaultReason {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentFaultReason::Offline => "offline",
            AgentFaultReason::Busy => "busy",
            AgentFaultReason::Timeout => "timeout",
            AgentFaultReason::Replaced => "replaced",
            AgentFaultReason::TooLarge => "too_large",
            AgentFaultReason::Protocol => "protocol",
        }
    }
}

impl std::fmt::Display for AgentFaultReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// CLI-reported local failures (`res_err.reason`) → the server's fault vocabulary.
pub fn fault_from_res_err_reason(reason: &str) -> AgentFaultReason {
    match reason {
        "connect_refused" => AgentFaultReason::Offline,
        "timeout" => AgentFaultReason::Timeout,
        _ => AgentFaultReason::Protocol,
    }
}

/// Wire-identical to the CLI's `ControlFrame` (`apps/cli/src/protocol.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        #[serde(default)]
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

pub fn encode_control_frame(frame: &ControlFrame) -> String {
    serde_json::to_string(frame).expect("control frames are always serializable")
}

fn frame_id(value: &serde_json::Value) -> Option<u32> {
    let n = value.get("id")?.as_f64()?;
    if n.is_finite() && n >= 0.0 && n <= u32::MAX as f64 && n.fract() == 0.0 {
        Some(n as u32)
    } else {
        None
    }
}

fn frame_headers(value: &serde_json::Value) -> std::collections::BTreeMap<String, String> {
    value
        .get("headers")
        .and_then(|h| h.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Parses a CLI → server control frame, tolerating the same sloppiness the
/// TypeScript `parseControlFrame` did: unknown frame types, wrong field types
/// and non-string header/model entries are dropped rather than throwing.
/// Server → CLI frames (`hello`, `req`) are not accepted from the wire.
pub fn parse_control_frame(text: &str) -> Option<ControlFrame> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if !value.is_object() {
        return None;
    }
    match value.get("t")?.as_str()? {
        "res" => {
            let id = frame_id(&value)?;
            let status = value.get("status")?.as_f64()?;
            if !status.is_finite() || status < 0.0 || status > u16::MAX as f64 {
                return None;
            }
            Some(ControlFrame::Res { id, status: status as u16, headers: frame_headers(&value) })
        }
        "res_end" => Some(ControlFrame::ResEnd { id: frame_id(&value)? }),
        "cancel" => Some(ControlFrame::Cancel { id: frame_id(&value)? }),
        "req_end" => Some(ControlFrame::ReqEnd { id: frame_id(&value)? }),
        "res_err" => Some(ControlFrame::ResErr {
            id: frame_id(&value)?,
            reason: value.get("reason")?.as_str()?.to_string(),
        }),
        "models" => {
            let models = value.get("models")?.as_array()?;
            Some(ControlFrame::Models {
                models: models.iter().filter_map(|m| m.as_str().map(str::to_string)).collect(),
            })
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryFrame<'a> {
    pub id: u32,
    pub kind: u8,
    pub chunk: &'a [u8],
}

pub fn encode_binary_frame(id: u32, kind: u8, chunk: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + chunk.len());
    out.extend_from_slice(&id.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(chunk);
    out
}

pub fn decode_binary_frame(data: &[u8]) -> Option<BinaryFrame<'_>> {
    if data.len() < 5 {
        return None;
    }
    Some(BinaryFrame {
        id: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        kind: data[4],
        chunk: &data[5..],
    })
}

/// Bounds for an agent-reported model list: ≤ 100 entries, each trimmed to
/// 1–128 chars, no whitespace (`/` allowed). An out-of-bounds report is
/// rejected whole (`None`) rather than partially applied.
pub fn validate_models_report(models: &[String]) -> Option<Vec<String>> {
    if models.len() > MAX_REPORTED_MODELS {
        return None;
    }
    let mut out = Vec::with_capacity(models.len());
    for raw in models {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.chars().count() > MAX_REPORTED_MODEL_ID_LENGTH {
            return None;
        }
        if trimmed.chars().any(char::is_whitespace) {
            return None;
        }
        out.push(trimmed.to_string());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tunnel_mux.test.ts § "binary framing".
    #[test]
    fn binary_frames_round_trip_and_reject_truncation() {
        let encoded = encode_binary_frame(70000, BODY_KIND_RESPONSE, b"hello");
        let decoded = decode_binary_frame(&encoded).unwrap();
        assert_eq!(decoded.id, 70000);
        assert_eq!(decoded.kind, BODY_KIND_RESPONSE);
        assert_eq!(decoded.chunk, b"hello");
        assert!(decode_binary_frame(&[0, 0, 0]).is_none());
    }

    /// tunnel_mux.test.ts § "control frame parsing".
    #[test]
    fn parses_known_frames_and_rejects_junk() {
        let res = parse_control_frame(r#"{"t":"res","id":1,"status":200,"headers":{"content-type":"a"}}"#).unwrap();
        match res {
            ControlFrame::Res { id, status, ref headers } => {
                assert_eq!((id, status), (1, 200));
                assert_eq!(headers.get("content-type").unwrap(), "a");
            }
            other => panic!("wrong frame: {other:?}"),
        }
        assert_eq!(parse_control_frame(r#"{"t":"res_end","id":2}"#), Some(ControlFrame::ResEnd { id: 2 }));
        assert!(parse_control_frame("not json").is_none());
        assert!(parse_control_frame(r#"{"t":"unknown"}"#).is_none());
        assert!(parse_control_frame(r#"{"t":"res","id":"x","status":200}"#).is_none());
        assert!(parse_control_frame("[1,2]").is_none());
        // Server → CLI frames are never accepted from the wire.
        assert!(parse_control_frame(r#"{"t":"hello","proto":1,"slug":"x"}"#).is_none());
        assert!(parse_control_frame(r#"{"t":"req","id":1,"method":"POST","path":"/models"}"#).is_none());
        // Non-string header values and model entries are dropped, not fatal.
        let res = parse_control_frame(r#"{"t":"res","id":1,"status":200,"headers":{"a":1,"b":"ok"}}"#).unwrap();
        assert!(matches!(res, ControlFrame::Res { ref headers, .. } if headers.len() == 1));
        assert_eq!(
            parse_control_frame(r#"{"t":"models","models":["a",3,"b"]}"#),
            Some(ControlFrame::Models { models: vec!["a".into(), "b".into()] })
        );
    }

    /// The frames the CLI parses must be exactly what we encode (apps/cli/src/protocol.rs).
    #[test]
    fn encodes_the_frames_the_cli_expects() {
        let hello = encode_control_frame(&ControlFrame::Hello { proto: AGENT_PROTO, slug: "my-mac".into() });
        assert_eq!(hello, r#"{"t":"hello","proto":1,"slug":"my-mac"}"#);
        let req = encode_control_frame(&ControlFrame::Req {
            id: 3,
            method: "POST".into(),
            path: "/chat/completions".into(),
            headers: [("content-type".to_string(), "application/json".to_string())].into_iter().collect(),
        });
        assert_eq!(
            req,
            r#"{"t":"req","id":3,"method":"POST","path":"/chat/completions","headers":{"content-type":"application/json"}}"#
        );
        assert_eq!(encode_control_frame(&ControlFrame::ReqEnd { id: 3 }), r#"{"t":"req_end","id":3}"#);
        assert_eq!(encode_control_frame(&ControlFrame::Cancel { id: 3 }), r#"{"t":"cancel","id":3}"#);
    }

    /// tunnel_mux.test.ts § "models report validation".
    #[test]
    fn bounds_model_reports_like_the_custom_manual_list() {
        let ok = vec!["llama3.3:70b".to_string(), "org/model".to_string()];
        assert_eq!(validate_models_report(&ok), Some(ok.clone()));
        assert_eq!(validate_models_report(&["has space".to_string()]), None);
        assert_eq!(validate_models_report(&[String::new()]), None);
        assert_eq!(validate_models_report(&["x".repeat(129)]), None);
        let many: Vec<String> = (0..101).map(|i| format!("m{i}")).collect();
        assert_eq!(validate_models_report(&many), None);
        // Entries are trimmed, and a 128-char id is still in bounds.
        assert_eq!(validate_models_report(&["  llama3  ".to_string()]), Some(vec!["llama3".to_string()]));
        assert_eq!(validate_models_report(&["x".repeat(128)]), Some(vec!["x".repeat(128)]));
    }

    /// cli/protocol.rs § "allowlist_by_format" — both ends enforce the same list.
    #[test]
    fn path_allowlist_by_format() {
        use CliProviderFormat::{Anthropic, OpenAI};
        assert!(is_allowed_path(OpenAI, "/chat/completions"));
        assert!(is_allowed_path(OpenAI, "/models"));
        assert!(is_allowed_path(OpenAI, "/audio/transcriptions"));
        assert!(!is_allowed_path(OpenAI, "/v1/messages"));
        assert!(is_allowed_path(Anthropic, "/v1/messages"));
        assert!(is_allowed_path(Anthropic, "/v1/messages/count_tokens"));
        assert!(is_allowed_path(Anthropic, "/v1/models"));
        assert!(!is_allowed_path(Anthropic, "/chat/completions"));
        assert!(!is_allowed_path(OpenAI, "/admin"));
        assert_eq!(CliProviderFormat::parse("openai"), Some(OpenAI));
        assert_eq!(CliProviderFormat::parse("gemini"), None);
    }

    #[test]
    fn res_err_reasons_map_to_faults() {
        assert_eq!(fault_from_res_err_reason("connect_refused"), AgentFaultReason::Offline);
        assert_eq!(fault_from_res_err_reason("timeout"), AgentFaultReason::Timeout);
        assert_eq!(fault_from_res_err_reason("aborted"), AgentFaultReason::Protocol);
        assert_eq!(fault_from_res_err_reason("whatever"), AgentFaultReason::Protocol);
    }
}
