//! `usage.cost` on Chat Completions responses (docs/api.md § Cost in usage, docs/pricing.md
//! § Cost in the response).
//!
//! The transport resolves the served model's price once and hands it both here and to the log
//! row, so the client's `usage.cost` and `request_logs.cost` are the same formula over the same
//! `usage` object. Only a top-level `usage` object is touched: a numeric `cost` already in it is
//! the upstream's own figure and stays; an unpriced request gets `null`, never `0`; a body or
//! chunk without `usage` is left alone.
//!
//! On a stream, every line is forwarded whole and only a line carrying `"usage"` is parsed. At
//! most one partial line is held (bounded by [`MAX_HELD_LINE`]); a longer line is forwarded as it
//! arrives and never annotated. Other lines pass byte for byte.

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::Value;

use crate::logging::usage_capture::from_openai_usage;
use crate::pricing::litellm::{compute_cost, CostUsage, ModelPrice};

/// Longest partial line held while waiting for its newline — the usage sniffer's own carry cap.
pub const MAX_HELD_LINE: usize = 256 * 1024;

const USAGE_MARKER: &[u8] = b"\"usage\"";

/// Inserts `cost` into `json.usage` when that is an object without a numeric `cost`. Returns
/// whether anything changed.
pub fn annotate_usage_cost(json: &mut Value, price: Option<&ModelPrice>) -> bool {
    let tokens = from_openai_usage(json.get("usage"));
    let Some(usage) = json.get_mut("usage").and_then(Value::as_object_mut) else { return false };
    if usage.get("cost").is_some_and(Value::is_number) {
        return false;
    }
    let cost = price.and_then(|price| {
        compute_cost(
            price,
            &CostUsage {
                prompt_tokens: tokens.prompt_tokens,
                completion_tokens: tokens.completion_tokens,
                cache_read_input_tokens: tokens.cache_read_input_tokens,
                cache_creation_input_tokens: tokens.cache_creation_input_tokens,
            },
        )
    });
    usage.insert("cost".into(), cost.map_or(Value::Null, Value::from));
    true
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// One SSE line without its `\n`. `Some` = the rewritten line, with the `data:` prefix and the
/// surrounding whitespace (a trailing `\r` included) kept as they were.
fn annotate_line(line: &[u8], price: Option<&ModelPrice>) -> Option<Vec<u8>> {
    if !contains(line, USAGE_MARKER) {
        return None;
    }
    let text = std::str::from_utf8(line).ok()?;
    let rest = text.strip_prefix("data:")?;
    let payload = rest.trim();
    if payload.is_empty() {
        return None;
    }
    let mut json: Value = serde_json::from_str(payload).ok()?;
    if !annotate_usage_cost(&mut json, price) {
        return None;
    }
    let leading = &rest[..rest.len() - rest.trim_start().len()];
    let trailing = &rest[rest.trim_end().len()..];
    Some(format!("data:{leading}{json}{trailing}").into_bytes())
}

/// The stream adapter; see the module docs.
pub struct AnnotateUsageCost<S> {
    source: S,
    price: Option<ModelPrice>,
    /// The partial line held since the last newline.
    held: Vec<u8>,
    /// The current line outgrew [`MAX_HELD_LINE`] and passes through unannotated until its newline.
    skipping: bool,
    pending: VecDeque<Bytes>,
    error: Option<io::Error>,
    done: bool,
}

pub fn annotate_usage_cost_stream<S>(source: S, price: Option<ModelPrice>) -> AnnotateUsageCost<S>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
{
    AnnotateUsageCost {
        source,
        price,
        held: Vec::new(),
        skipping: false,
        pending: VecDeque::new(),
        error: None,
        done: false,
    }
}

impl<S> AnnotateUsageCost<S> {
    fn push(&mut self, bytes: Bytes) {
        if !bytes.is_empty() {
            self.pending.push_back(bytes);
        }
    }

    /// One whole line including its `\n` (or the unterminated last line at EOF).
    fn push_line(&mut self, line: Bytes) {
        let body = line.strip_suffix(b"\n").unwrap_or(&line);
        match annotate_line(body, self.price.as_ref()) {
            Some(mut rewritten) => {
                if line.ends_with(b"\n") {
                    rewritten.push(b'\n');
                }
                self.push(Bytes::from(rewritten));
            }
            None => self.push(line),
        }
    }

    fn hold(&mut self, tail: &[u8]) {
        self.held.extend_from_slice(tail);
        if self.held.len() > MAX_HELD_LINE {
            let held = Bytes::from(std::mem::take(&mut self.held));
            self.push(held);
            self.skipping = true;
        }
    }

    fn transform(&mut self, chunk: Bytes) {
        let mut offset = 0;
        // Finish the line in progress first.
        if self.skipping || !self.held.is_empty() {
            match chunk.iter().position(|b| *b == b'\n') {
                None if self.skipping => return self.push(chunk),
                None => return self.hold(&chunk),
                Some(nl) if self.skipping => {
                    self.push(chunk.slice(..=nl));
                    self.skipping = false;
                    offset = nl + 1;
                }
                Some(nl) => {
                    self.held.extend_from_slice(&chunk[..=nl]);
                    let line = Bytes::from(std::mem::take(&mut self.held));
                    self.push_line(line);
                    offset = nl + 1;
                }
            }
        }
        let rest = chunk.slice(offset..);
        let Some(last) = rest.iter().rposition(|b| *b == b'\n') else { return self.hold(&rest) };
        let whole = rest.slice(..=last);
        if contains(&whole, USAGE_MARKER) {
            let mut start = 0;
            while start < whole.len() {
                let end = start + whole[start..].iter().position(|b| *b == b'\n').expect("ends with a newline") + 1;
                let line = whole.slice(start..end);
                if contains(&line, USAGE_MARKER) {
                    self.push_line(line);
                } else {
                    self.push(line);
                }
                start = end;
            }
        } else {
            // No usage anywhere in these lines: forwarded as one zero-copy slice.
            self.push(whole);
        }
        let tail = rest.slice(last + 1..);
        self.hold(&tail);
    }

    fn flush_held(&mut self, annotate: bool) {
        let held = Bytes::from(std::mem::take(&mut self.held));
        if annotate && !self.skipping {
            self.push_line(held);
        } else {
            self.push(held);
        }
    }
}

impl<S> Stream for AnnotateUsageCost<S>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        loop {
            if let Some(bytes) = this.pending.pop_front() {
                return Poll::Ready(Some(Ok(bytes)));
            }
            if let Some(error) = this.error.take() {
                this.done = true;
                return Poll::Ready(Some(Err(error)));
            }
            if this.done {
                return Poll::Ready(None);
            }
            match this.source.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => this.transform(chunk),
                Poll::Ready(Some(Err(error))) => {
                    // What already arrived goes out before the error, as it did unwrapped.
                    this.flush_held(false);
                    this.error = Some(error);
                }
                Poll::Ready(None) => {
                    this.flush_held(true);
                    this.done = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn price() -> ModelPrice {
        ModelPrice { input: 0.000_01, output: 0.000_02, cache_read: Some(0.000_001), cache_creation: None, source: None }
    }

    async fn run(chunks: Vec<&'static str>, price: Option<ModelPrice>) -> Vec<Bytes> {
        let source = futures::stream::iter(chunks.into_iter().map(|c| Ok::<_, io::Error>(Bytes::from_static(c.as_bytes()))));
        annotate_usage_cost_stream(source, price).map(|r| r.expect("chunk")).collect().await
    }

    fn joined(out: &[Bytes]) -> String {
        out.iter().map(|b| String::from_utf8_lossy(b).into_owned()).collect()
    }

    #[test]
    fn cost_follows_the_formula_on_the_normalized_usage() {
        let mut body = json!({ "usage": { "prompt_tokens": 1000, "completion_tokens": 100, "prompt_tokens_details": { "cached_tokens": 400 } } });
        assert!(annotate_usage_cost(&mut body, Some(&price())));
        let expected = compute_cost(
            &price(),
            &CostUsage { prompt_tokens: Some(1000), completion_tokens: Some(100), cache_read_input_tokens: Some(400), cache_creation_input_tokens: None },
        )
        .unwrap();
        assert_eq!(body["usage"]["cost"].as_f64(), Some(expected));
        // 600 uncached at 1e-5, 400 cached at 1e-6, 100 output at 2e-5.
        assert!((expected - 0.0084).abs() < 1e-12);
    }

    #[test]
    fn unpriced_or_tokenless_usage_is_null_and_a_missing_usage_is_never_created() {
        let mut body = json!({ "usage": { "prompt_tokens": 5 } });
        assert!(annotate_usage_cost(&mut body, None));
        assert_eq!(body["usage"]["cost"], Value::Null);

        let mut body = json!({ "usage": {} });
        assert!(annotate_usage_cost(&mut body, Some(&price())));
        assert_eq!(body["usage"]["cost"], Value::Null);

        for mut body in [json!({ "choices": [] }), json!({ "usage": null }), json!([1, 2])] {
            let before = body.clone();
            assert!(!annotate_usage_cost(&mut body, Some(&price())));
            assert_eq!(body, before);
        }
    }

    #[test]
    fn an_upstream_numeric_cost_is_never_overwritten_but_a_null_one_is_filled() {
        let mut body = json!({ "usage": { "prompt_tokens": 10, "completion_tokens": 1, "cost": 0.5 } });
        assert!(!annotate_usage_cost(&mut body, Some(&price())));
        assert_eq!(body["usage"]["cost"], json!(0.5));

        let mut body = json!({ "usage": { "prompt_tokens": 10, "completion_tokens": 1, "cost": null } });
        assert!(annotate_usage_cost(&mut body, Some(&price())));
        assert!(body["usage"]["cost"].is_number());
    }

    #[tokio::test]
    async fn only_the_usage_line_changes_and_every_other_byte_is_kept() {
        let input = vec![
            ": keepalive\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1000,\"completion_tokens\":100}}\r\n\r\n",
            "data: [DONE]\n\n",
        ];
        let out = run(input.clone(), Some(price())).await;
        let text = joined(&out);
        let expected = format!(
            "{}{}data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":1000,\"completion_tokens\":100,\"cost\":{}}}}}\r\n\r\n{}",
            input[0],
            input[1],
            Value::from(
                compute_cost(&price(), &CostUsage { prompt_tokens: Some(1000), completion_tokens: Some(100), ..Default::default() })
                    .unwrap()
            ),
            input[3]
        );
        assert_eq!(text, expected);
        // Lines without usage went out as the same chunks they arrived in.
        assert_eq!(out[0], Bytes::from_static(input[0].as_bytes()));
        assert_eq!(out[1], Bytes::from_static(input[1].as_bytes()));
    }

    #[tokio::test]
    async fn a_usage_line_split_across_chunks_is_reassembled_before_it_is_annotated() {
        let out = run(
            vec!["data: {\"choices\":[],\"us", "age\":{\"prompt_tokens\":1", "0}}\n", "\ndata: [DONE]\n\n"],
            None,
        )
        .await;
        assert_eq!(
            joined(&out),
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"cost\":null}}\n\ndata: [DONE]\n\n"
        );
    }

    #[tokio::test]
    async fn a_line_past_the_hold_cap_passes_through_unannotated() {
        let long: &'static str = Box::leak(
            format!("data: {{\"pad\":\"{}\",\"usage\":{{\"prompt_tokens\":1}}}}\n\n", "x".repeat(MAX_HELD_LINE + 10)).into_boxed_str(),
        );
        // The cap is crossed inside the second chunk; the line ends in the third.
        let (a, rest) = long.split_at(MAX_HELD_LINE / 2);
        let (b, c) = rest.split_at(MAX_HELD_LINE / 2 + 5);
        let out = run(vec![a, b, c, "data: [DONE]\n\n"], Some(price())).await;
        assert_eq!(joined(&out), format!("{long}data: [DONE]\n\n"));
    }

    #[tokio::test]
    async fn an_upstream_error_still_delivers_what_arrived_first() {
        let source = futures::stream::iter(vec![
            Ok(Bytes::from_static(b"data: {\"choices\":[]}\ndata: {\"par")),
            Err(io::Error::other("reset")),
        ]);
        let items: Vec<_> = annotate_usage_cost_stream(source, None).collect().await;
        let text: String = items.iter().filter_map(|r| r.as_ref().ok()).map(|b| String::from_utf8_lossy(b).into_owned()).collect();
        assert_eq!(text, "data: {\"choices\":[]}\ndata: {\"par");
        assert!(items.last().unwrap().is_err());
    }
}
