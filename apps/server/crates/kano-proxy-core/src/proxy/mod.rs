//! Protocol conversion, SSE handling and dispatch (docs/api.md).

pub mod backpressure;
pub mod codex_openai;
pub mod dispatch;
pub mod dispatch_anthropic_via_openai;
pub mod dispatch_audio;
pub mod dispatch_walk;
pub mod gemini_anthropic;
pub mod gemini_openai;
pub mod gemini_wire;
pub mod grok_anthropic;
pub mod openai_anthropic;
pub mod request_json;
pub mod responses_openai;
pub mod sse;
pub mod sse_error_rewrite;
pub mod sse_lines;
pub mod wire;
pub mod wire_anthropic;
pub mod wire_openai;
