//! Rewriting upstream errors inside a live stream (docs/api.md § In-stream errors).
//!
//! Rewrites only the proxy's own `data: {"error"…}` lines into another surface's shape, while
//! every ordinary line is forwarded byte for byte: a line is classified from a 14-byte prefix,
//! ordinary bytes stream straight through (a 48 MiB line never waits for its delimiter), and
//! only a proxy error is buffered — capped at [`MAX_PROXY_ERROR_BYTES`].

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::stream::Stream;

/// The two spellings the proxy's own error frames use.
const PREFIXES: [&[u8]; 2] = [b"data: {\"error\"", b"data:{\"error\""];
const PREFIX_CAPACITY: usize = 14;

pub const MAX_PROXY_ERROR_BYTES: usize = 64 * 1024;

/// The frame substituted for a proxy error too large to buffer (rewritten like any other).
const OVERSIZED_ERROR_LINE: &str =
    r#"data: {"error":{"code":"upstream_error","message":"Proxy error exceeds the 64 KiB rewrite limit"}}"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Prefix,
    Pass,
    Error,
    Skip,
}

pub struct RewriteSseErrors<S, F> {
    source: S,
    rewrite: F,
    state: State,
    prefix: [u8; PREFIX_CAPACITY],
    prefix_size: usize,
    error: Vec<u8>,
    done: bool,
    flushed: bool,
    pending: VecDeque<Bytes>,
}

/// `rewriteSseErrors(source, rewrite)`. `rewrite` receives the whole error line (`data: …`,
/// without its newline) and returns the replacement line; returning the input unchanged makes
/// the original bytes pass through untouched.
pub fn rewrite_sse_errors<S, F>(source: S, rewrite: F) -> RewriteSseErrors<S, F>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
    F: FnMut(&str) -> String,
{
    RewriteSseErrors {
        source,
        rewrite,
        state: State::Prefix,
        prefix: [0u8; PREFIX_CAPACITY],
        prefix_size: 0,
        error: Vec::new(),
        done: false,
        flushed: false,
        pending: VecDeque::new(),
    }
}

impl<S, F> RewriteSseErrors<S, F>
where
    F: FnMut(&str) -> String,
{
    fn reset(&mut self) {
        self.state = State::Prefix;
        self.prefix_size = 0;
        self.error = Vec::new();
    }

    fn enqueue(&mut self, bytes: Bytes) {
        if !bytes.is_empty() {
            self.pending.push_back(bytes);
        }
    }

    /// `endLine(controller, newline)`.
    fn end_line(&mut self, newline: bool) {
        if self.state == State::Prefix && self.prefix_size > 0 {
            let held = Bytes::copy_from_slice(&self.prefix[..self.prefix_size]);
            self.enqueue(held);
        }
        if self.state == State::Error {
            let original = String::from_utf8_lossy(&self.error).into_owned();
            let rewritten = (self.rewrite)(&original);
            if rewritten == original {
                let raw = Bytes::from(std::mem::take(&mut self.error));
                self.enqueue(raw);
                if newline {
                    self.enqueue(Bytes::from_static(b"\n"));
                }
            } else {
                let mut out = rewritten;
                if newline {
                    out.push('\n');
                }
                self.enqueue(Bytes::from(out.into_bytes()));
            }
        }
        self.reset();
    }

    fn transform(&mut self, chunk: Bytes) {
        let mut offset = 0usize;
        while offset < chunk.len() {
            let newline = chunk[offset..].iter().position(|b| *b == b'\n').map(|i| offset + i);
            let end = newline.unwrap_or(chunk.len());
            while offset < end && self.state == State::Prefix {
                self.prefix[self.prefix_size] = chunk[offset];
                self.prefix_size += 1;
                offset += 1;
                let size = self.prefix_size;
                let seen = Bytes::copy_from_slice(&self.prefix[..size]);
                let matching = |p: &&&[u8]| size <= p.len() && p.starts_with(&seen);
                let any_candidate = PREFIXES.iter().filter(matching).count() > 0;
                let completed = PREFIXES.iter().filter(matching).any(|p| p.len() == size);
                if !any_candidate {
                    self.state = State::Pass;
                    self.enqueue(seen);
                } else if completed {
                    self.state = State::Error;
                    self.error = seen.to_vec();
                }
            }
            if self.state == State::Pass {
                if end > offset || newline.is_some() {
                    // Include the delimiter without decoding/re-encoding normal bytes.
                    let stop = end + usize::from(newline.is_some());
                    self.enqueue(chunk.slice(offset..stop));
                }
            } else if self.state == State::Error {
                if self.error.len() + end - offset > MAX_PROXY_ERROR_BYTES {
                    let mut replacement = (self.rewrite)(OVERSIZED_ERROR_LINE);
                    replacement.push('\n');
                    self.enqueue(Bytes::from(replacement.into_bytes()));
                    self.error = Vec::new();
                    self.state = State::Skip;
                } else {
                    self.error.extend_from_slice(&chunk[offset..end]);
                }
            }
            if let Some(nl) = newline {
                if self.state == State::Prefix {
                    if self.prefix_size > 0 {
                        let held = Bytes::copy_from_slice(&self.prefix[..self.prefix_size]);
                        self.enqueue(held);
                    }
                    self.enqueue(chunk.slice(nl..nl + 1));
                    self.reset();
                } else {
                    self.end_line(true);
                }
            }
            offset = end + usize::from(newline.is_some());
        }
    }
}

impl<S, F> Stream for RewriteSseErrors<S, F>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
    F: FnMut(&str) -> String + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(chunk) = this.pending.pop_front() {
                return Poll::Ready(Some(Ok(chunk)));
            }
            if this.done {
                if this.flushed {
                    return Poll::Ready(None);
                }
                this.flushed = true;
                this.end_line(false);
                continue;
            }
            match Pin::new(&mut this.source).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => this.done = true,
                Poll::Ready(Some(Err(err))) => {
                    this.done = true;
                    this.flushed = true;
                    return Poll::Ready(Some(Err(err)));
                }
                Poll::Ready(Some(Ok(chunk))) => this.transform(chunk),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::sse_lines::chunked_bytes;
    use futures::StreamExt;

    /// Stands in for `rewriteOpenAIErrorFramesToResponses`: parses the line's JSON and re-frames
    /// it, leaving anything that is not JSON exactly as it arrived.
    fn to_responses(line: &str) -> String {
        let payload = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")).unwrap_or(line);
        match serde_json::from_str::<serde_json::Value>(payload) {
            Ok(value) => format!("event: response.failed\ndata: {value}"),
            Err(_) => line.to_string(),
        }
    }

    async fn run(input: &str, size: usize) -> String {
        let mut out = Vec::new();
        let mut stream = rewrite_sse_errors(chunked_bytes(input, size), to_responses);
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.unwrap());
        }
        String::from_utf8(out).unwrap()
    }

    #[tokio::test]
    async fn preserves_ordinary_unicode_crlf_and_eof_bytes_at_every_chunk_size() {
        let original = "event: response.created\r\ndata: {\"type\":\"response.created\",\"text\":\"台灣🙂\"}\r\n\ndata: {\"errors\":[]}\n: comment\nlast";
        for size in [1usize, 2, 7, 13, 64] {
            assert_eq!(run(original, size).await, original, "chunk size {size}");
        }
    }

    #[tokio::test]
    async fn rewrites_split_errors_and_resumes_ordinary_forwarding() {
        let input = "data:{\"error\":{\"message\":\"台灣🙂\",\"code\":\"request_too_large\"}}\n\ndata: {\"type\":\"unchanged\"}\n\n";
        for size in [1usize, 2, 7, 64] {
            let result = run(input, size).await;
            assert!(result.contains("event: response.failed\ndata: "), "{result}");
            assert!(result.contains("\"message\":\"台灣🙂\""), "{result}");
            assert!(result.contains("\"code\":\"request_too_large\""), "{result}");
            assert!(result.contains("\n\ndata: {\"type\":\"unchanged\"}\n\n"), "{result}");
        }
    }

    #[tokio::test]
    async fn bounds_oversized_proxy_errors_without_leaking_their_payload() {
        let input = format!(
            "data: {{\"error\":{{\"message\":\"{}\"}}}}\n\ndata: {{\"type\":\"unchanged\"}}\n",
            "x".repeat(MAX_PROXY_ERROR_BYTES + 1)
        );
        let result = run(&input, 1024).await;
        assert!(result.len() < 1024, "{}", result.len());
        assert!(result.contains("response.failed"));
        assert!(result.contains("64 KiB"));
        assert!(result.contains("data: {\"type\":\"unchanged\"}"));
    }

    #[tokio::test]
    async fn preserves_malformed_error_looking_bytes_instead_of_re_encoding_them() {
        let mut original = b"data: {\"error\":".to_vec();
        original.push(255);
        original.push(b'\n');
        let source = futures::stream::once(async move { Ok(Bytes::from(original.clone())) });
        let mut stream = rewrite_sse_errors(Box::pin(source), to_responses);
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.unwrap());
        }
        let mut expected = b"data: {\"error\":".to_vec();
        expected.push(255);
        expected.push(b'\n');
        assert_eq!(out, expected);
    }

    #[tokio::test]
    async fn forwards_a_long_ordinary_line_before_its_delimiter_arrives() {
        // 8 MiB of an ordinary line reaches the client without ever being buffered whole.
        let block = Bytes::from(vec![b'A'; 64 * 1024]);
        let prefix = Bytes::from_static(b"data: {\"type\":\"response.completed\",\"payload\":\"");
        let suffix = Bytes::from_static(b"\"}\n\n");
        let blocks = 128usize;
        let mut chunks: Vec<Result<Bytes, io::Error>> = vec![Ok(prefix.clone())];
        chunks.extend((0..blocks).map(|_| Ok(block.clone())));
        chunks.push(Ok(suffix.clone()));
        let mut stream = rewrite_sse_errors(Box::pin(futures::stream::iter(chunks)), to_responses);
        let first = stream.next().await.unwrap().unwrap();
        assert!(!first.is_empty());
        let mut total = first.len();
        while let Some(chunk) = stream.next().await {
            total += chunk.unwrap().len();
        }
        assert_eq!(total, prefix.len() + blocks * block.len() + suffix.len());
    }

    #[tokio::test]
    async fn an_unterminated_error_line_is_still_rewritten_at_eof() {
        let result = run("data: {\"error\":{\"message\":\"x\"}}", 3).await;
        assert!(result.starts_with("event: response.failed\ndata: "), "{result}");
        assert!(!result.ends_with('\n'), "{result}");
    }
}
