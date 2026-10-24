//! A group endpoint's model name on the native Anthropic passthrough (docs/api.md § Group
//! endpoints).
//!
//! The upstream answers under the target's own id; a group endpoint answers under the name the
//! client sent. On a stream only the first `data:` line is parsed — `message_start` carries the
//! message's `model` — and at most [`MAX_HELD`] bytes are held waiting for it; every byte after it
//! passes through untouched. A non-stream body is one JSON object whose top-level `model` is
//! rewritten.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::stream::{Fuse, Stream, StreamExt};
use serde_json::Value;

/// Longest prefix held while looking for the first `data:` line; past it the stream passes
/// through unrewritten.
pub const MAX_HELD: usize = 64 * 1024;

/// Sets the top-level `model` of a JSON object that already names one. Returns whether anything
/// changed.
fn set_model(json: &mut Value, model: &str) -> bool {
    match json.get_mut("model") {
        Some(Value::String(current)) if current != model => {
            *current = model.to_string();
            true
        }
        _ => false,
    }
}

/// A successful non-stream Messages body with its `model` replaced; anything that is not a JSON
/// object naming a model comes back unchanged.
pub fn echo_model_body(body: Bytes, model: &str) -> Bytes {
    let Ok(mut json) = serde_json::from_slice::<Value>(&body) else { return body };
    if !set_model(&mut json, model) {
        return body;
    }
    Bytes::from(json.to_string())
}

/// One `data:` line without its `\n`. `Some` = the rewritten line, with the `data:` prefix and
/// the surrounding whitespace (a trailing `\r` included) kept as they were.
fn echo_line(line: &[u8], model: &str) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(line).ok()?;
    let rest = text.strip_prefix("data:")?;
    let mut json: Value = serde_json::from_str(rest.trim()).ok()?;
    if json.get("type").and_then(Value::as_str) != Some("message_start") {
        return None;
    }
    if !set_model(json.get_mut("message")?, model) {
        return None;
    }
    let leading = &rest[..rest.len() - rest.trim_start().len()];
    let trailing = &rest[rest.trim_end().len()..];
    Some(format!("data:{leading}{json}{trailing}").into_bytes())
}

/// The stream adapter; see the module docs.
pub struct EchoModel<S> {
    source: Fuse<S>,
    model: String,
    /// Bytes read before the first `data:` line was complete.
    held: Vec<u8>,
    /// The first `data:` line has been handled; everything now passes through.
    done: bool,
    /// An upstream error that arrived while bytes were held, yielded after them.
    error: Option<io::Error>,
}

pub fn echo_model_stream<S>(source: S, model: String) -> EchoModel<S>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
{
    EchoModel { source: source.fuse(), model, held: Vec::new(), done: false, error: None }
}

impl<S> EchoModel<S> {
    /// `Some` = the held bytes to release (the first `data:` line rewritten when it is a
    /// `message_start`); `None` = keep reading.
    fn release(&mut self) -> Option<Bytes> {
        let mut start = 0;
        while let Some(nl) = self.held[start..].iter().position(|b| *b == b'\n') {
            let end = start + nl;
            if self.held[start..end].starts_with(b"data:") {
                let held = std::mem::take(&mut self.held);
                let Some(line) = echo_line(&held[start..end], &self.model) else { return Some(Bytes::from(held)) };
                let mut out = Vec::with_capacity(held.len() + self.model.len());
                out.extend_from_slice(&held[..start]);
                out.extend_from_slice(&line);
                out.extend_from_slice(&held[end..]);
                return Some(Bytes::from(out));
            }
            start = end + 1;
        }
        (self.held.len() > MAX_HELD).then(|| Bytes::from(std::mem::take(&mut self.held)))
    }
}

impl<S> Stream for EchoModel<S>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        if let Some(err) = this.error.take() {
            return Poll::Ready(Some(Err(err)));
        }
        if this.done {
            return this.source.poll_next_unpin(cx);
        }
        loop {
            match this.source.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    this.held.extend_from_slice(&chunk);
                    if let Some(out) = this.release() {
                        this.done = true;
                        return Poll::Ready(Some(Ok(out)));
                    }
                }
                Poll::Ready(end) => {
                    // EOF or an error before the first `data:` line: the held bytes go out as
                    // they are, then the error (if any).
                    this.done = true;
                    this.error = end.and_then(Result::err);
                    if this.held.is_empty() {
                        return Poll::Ready(this.error.take().map(Err));
                    }
                    return Poll::Ready(Some(Ok(Bytes::from(std::mem::take(&mut this.held)))));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    async fn collect(chunks: Vec<&'static [u8]>) -> Vec<u8> {
        let source = stream::iter(chunks.into_iter().map(|c| Ok(Bytes::from_static(c))));
        let out: Vec<_> = echo_model_stream(source, "claude-sonnet-5.5".into()).collect().await;
        out.into_iter().flat_map(|r| r.unwrap().to_vec()).collect()
    }

    #[tokio::test]
    async fn message_start_split_across_chunks_names_the_group_model() {
        let out = collect(vec![
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"mod",
            b"el\":\"claude-sonnet-5-5\",\"usage\":{\"input_tokens\":3}}}\n\nevent: ping\n",
            b"data: {\"type\":\"ping\",\"model\":\"claude-sonnet-5-5\"}\n\n",
        ])
        .await;
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"claude-sonnet-5.5\",\"usage\":{\"input_tokens\":3}}}\n\nevent: ping\ndata: {\"type\":\"ping\",\"model\":\"claude-sonnet-5-5\"}\n\n"
        );
    }

    #[tokio::test]
    async fn a_first_event_that_is_not_message_start_passes_byte_for_byte() {
        let input: &[u8] = b"event: error\ndata: {\"type\":\"error\",\"model\":\"x\"}\n\n";
        assert_eq!(collect(vec![input]).await, input);
    }

    #[tokio::test]
    async fn a_prefix_past_the_cap_passes_unrewritten() {
        let comment: &'static [u8] = Box::leak(vec![b':'; MAX_HELD + 1].into_boxed_slice());
        let line: &[u8] = b"\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-5-5\"}}\n";
        let out = collect(vec![comment, line]).await;
        assert_eq!(out.len(), comment.len() + line.len());
        assert!(out.ends_with(line));
    }

    #[tokio::test]
    async fn bytes_held_at_an_upstream_error_go_out_before_it() {
        let source = stream::iter(vec![Ok(Bytes::from_static(b"event: message_start\n")), Err(io::Error::other("cut"))]);
        let out: Vec<_> = echo_model_stream(source, "m".into()).collect().await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].as_ref().unwrap().as_ref(), b"event: message_start\n");
        assert!(out[1].is_err());
    }

    #[test]
    fn a_non_stream_body_names_the_group_model() {
        let body = Bytes::from_static(br#"{"id":"m","type":"message","model":"claude-sonnet-5-5","content":[]}"#);
        assert_eq!(
            echo_model_body(body, "claude-sonnet-5.5").as_ref(),
            br#"{"id":"m","type":"message","model":"claude-sonnet-5.5","content":[]}"#
        );
        let not_json = Bytes::from_static(b"not json");
        assert_eq!(echo_model_body(not_json.clone(), "claude-sonnet-5.5"), not_json);
    }
}
