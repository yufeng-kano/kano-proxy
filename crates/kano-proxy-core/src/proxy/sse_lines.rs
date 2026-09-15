//! Port of apps/api/src/proxy/sse_lines.ts (docs/api.md § Streaming).
//!
//! Splits a byte stream into SSE lines incrementally: each byte is scanned once, each line
//! joined once, unterminated EOF lines included, and no more than one line is ever held in
//! memory. A line longer than `MAX_SSE_LINE_BYTES` fails the stream instead of buffering it.
//!
//! The TypeScript reader is demand-driven through `DemandReader.ready()`; a Rust `Stream` is
//! pull-based by construction, so the adapter inherits that flow control for free — nothing
//! is read from the upstream until the consumer polls.

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::stream::Stream;

/// Parsed SSE lines have a finite byte budget; native byte passthrough does not use this reader.
pub const MAX_SSE_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Message of the TypeScript `SseLineTooLargeError`, kept verbatim so callers (and tests) can
/// recognize the failure through the `io::Error` the stream yields.
pub const SSE_LINE_TOO_LARGE_MESSAGE: &str = "Upstream SSE event exceeds the 16 MiB parsing limit";

/// The error a line past the budget yields; `SseLineTooLargeError` has no Rust equivalent, so
/// the stream's own error type carries it.
pub fn sse_line_too_large_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, SSE_LINE_TOO_LARGE_MESSAGE)
}

/// True when `err` is the line-budget failure (matching `instanceof SseLineTooLargeError`).
pub fn is_sse_line_too_large(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::InvalidData && err.to_string() == SSE_LINE_TOO_LARGE_MESSAGE
}

/// Line splitter over a byte stream. Lines exclude the `\n`; a trailing `\r` is kept, exactly
/// as the TypeScript reader does (callers `trim()` the payload themselves).
pub struct SseLines<S> {
    source: S,
    /// Bytes of the line being assembled; decoded once, at the delimiter.
    line: Vec<u8>,
    max_bytes: usize,
    done: bool,
    pending: VecDeque<String>,
}

/// `readSseLines(source, maxBytes)`.
pub fn read_sse_lines<S>(source: S, max_bytes: usize) -> SseLines<S>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
{
    SseLines { source, line: Vec::new(), max_bytes, done: false, pending: VecDeque::new() }
}

/// `readSseLines(source)` with the default 16 MiB budget.
pub fn read_sse_lines_default<S>(source: S) -> SseLines<S>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
{
    read_sse_lines(source, MAX_SSE_LINE_BYTES)
}

impl<S> SseLines<S> {
    /// Scans one upstream chunk, queueing every completed line. `Err` means the budget was
    /// exceeded, which ends the stream.
    fn push_chunk(&mut self, chunk: &[u8]) -> Result<(), io::Error> {
        let mut start = 0usize;
        while start < chunk.len() {
            let newline = chunk[start..].iter().position(|b| *b == b'\n').map(|i| start + i);
            let end = newline.unwrap_or(chunk.len());
            if self.line.len() + (end - start) > self.max_bytes {
                return Err(sse_line_too_large_error());
            }
            self.line.extend_from_slice(&chunk[start..end]);
            match newline {
                Some(nl) => {
                    let line = String::from_utf8_lossy(&self.line).into_owned();
                    self.line.clear();
                    self.pending.push_back(line);
                    start = nl + 1;
                }
                None => start = chunk.len(),
            }
        }
        Ok(())
    }
}

impl<S> Stream for SseLines<S>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
{
    type Item = Result<String, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(line) = this.pending.pop_front() {
                return Poll::Ready(Some(Ok(line)));
            }
            if this.done {
                return Poll::Ready(None);
            }
            match Pin::new(&mut this.source).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    this.done = true;
                    if !this.line.is_empty() {
                        let tail = String::from_utf8_lossy(&this.line).into_owned();
                        this.line.clear();
                        this.pending.push_back(tail);
                    }
                }
                Poll::Ready(Some(Err(err))) => {
                    this.done = true;
                    return Poll::Ready(Some(Err(err)));
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    if let Err(err) = this.push_chunk(&chunk) {
                        this.done = true;
                        this.line.clear();
                        return Poll::Ready(Some(Err(err)));
                    }
                }
            }
        }
    }
}

/// Test/stub helper: a byte stream that serves `text` in `size`-byte chunks, so converters can
/// be fed on awkward boundaries the way the vitest suites do.
pub fn chunked_bytes(text: &str, size: usize) -> crate::upstream::transport::ByteStream {
    let bytes = Bytes::copy_from_slice(text.as_bytes());
    let mut chunks: Vec<Bytes> = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let end = (offset + size).min(bytes.len());
        chunks.push(bytes.slice(offset..end));
        offset = end;
    }
    Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    async fn collect_lines(text: &str, size: usize, max: usize) -> Result<Vec<String>, io::Error> {
        let mut out = Vec::new();
        let mut lines = read_sse_lines(chunked_bytes(text, size), max);
        while let Some(line) = lines.next().await {
            out.push(line?);
        }
        Ok(out)
    }

    #[tokio::test]
    async fn decodes_split_utf8_crlf_empty_lines_and_an_unterminated_final_line() {
        let out = collect_lines("data: 台灣🙂\r\n\ndata: 結束", 1, MAX_SSE_LINE_BYTES).await.unwrap();
        assert_eq!(out, vec!["data: 台灣🙂\r".to_string(), String::new(), "data: 結束".to_string()]);
    }

    #[tokio::test]
    async fn enforces_byte_size_even_for_a_complete_line_in_a_single_read() {
        let err = collect_lines("台灣🙂\n", 64, 9).await.unwrap_err();
        assert!(is_sse_line_too_large(&err), "{err}");
    }

    #[tokio::test]
    async fn allows_an_exact_byte_boundary() {
        let out = collect_lines("台灣🙂\n", 2, 10).await.unwrap();
        assert_eq!(out, vec!["台灣🙂".to_string()]);
    }

    #[tokio::test]
    async fn counts_each_line_against_the_budget_independently() {
        let out = collect_lines("12345\n12345\n12345", 3, 5).await.unwrap();
        assert_eq!(out, vec!["12345", "12345", "12345"]);
    }

    #[tokio::test]
    async fn stops_reading_when_the_consumer_stops_pulling() {
        // A Rust stream is pull-based: nothing past the first line is read before it is polled.
        let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = served.clone();
        let source = futures::stream::repeat_with(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Bytes::from_static(b"data: next\n"))
        });
        let mut lines = read_sse_lines(Box::pin(source), MAX_SSE_LINE_BYTES);
        let first = lines.next().await.unwrap().unwrap();
        assert_eq!(first, "data: next");
        assert!(served.load(std::sync::atomic::Ordering::SeqCst) <= 2);
    }

    #[tokio::test]
    async fn propagates_an_upstream_error() {
        let source = futures::stream::iter(vec![
            Ok(Bytes::from_static(b"data: a\n")),
            Err(io::Error::other("boom")),
        ]);
        let mut lines = read_sse_lines(Box::pin(source), MAX_SSE_LINE_BYTES);
        assert_eq!(lines.next().await.unwrap().unwrap(), "data: a");
        let err = lines.next().await.unwrap().unwrap_err();
        assert_eq!(err.to_string(), "boom");
        assert!(lines.next().await.is_none());
    }
}
