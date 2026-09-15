//! Port of apps/api/src/proxy/sse.ts (docs/api.md § Streaming, § Keepalive and idle timeout,
//! § Eager streaming commit).
//!
//! Two relays, both of which never buffer an upstream stream:
//!
//! - [`stream_with_keepalive`] pipes an upstream body, injecting `: keepalive` comments across
//!   every silence gap for the whole stream lifetime, and tearing the connection down with a
//!   terminal stall frame after `idle_timeout` of upstream silence.
//! - [`stream_with_eager_producer`] commits the SSE response before the candidate walk even
//!   starts: keepalives fire from t=0 while `run` acquires an account and reaches upstream,
//!   then `pipe_upstream` relays the body under the same rules.
//!
//! The TypeScript demand gate (`controller.desiredSize <= 0` → park) is
//! [`crate::proxy::backpressure`]'s bounded channel here: `Emitter::enqueue` parks while the
//! client is not consuming, so timers pause with the pump exactly as they did — a client stall
//! is not an upstream silence gap.
//!
//! `on_close` fires exactly once, whatever ends the stream, including a client that simply
//! dropped the response: the pump holds a close guard whose `Drop` reports `Cancel` unless the
//! pump already recorded a reason. That is what keeps a log row (and a pool-extension lease)
//! from being lost when the connection goes away.

use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use tokio::time::Instant;

use crate::proxy::backpressure::{backpressured_byte_stream, Emitter};
use crate::upstream::transport::ByteStream;

/// `sseKeepaliveComment()`.
pub fn sse_keepalive_comment() -> Bytes {
    Bytes::from_static(b": keepalive\n\n")
}

/// The documented keepalive cadence: every 10s from t=0 (docs/api.md).
pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// Why the outgoing stream ended — passed to `on_close` for logging (docs/logging.md
/// "Streaming rows").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamCloseReason {
    Done,
    Cancel,
    Error,
    IdleTimeout,
}

/// Called with every real upstream chunk, in order, right after it is enqueued — never for the
/// keepalive comments. A tap that panics would poison the stream, so taps do capture only.
pub type Tap = Arc<dyn Fn(&Bytes) + Send + Sync>;
/// Called exactly once when the stream ends.
pub type OnClose = Box<dyn FnOnce(StreamCloseReason) + Send>;

#[derive(Default)]
pub struct StreamKeepaliveOpts {
    pub tap: Option<Tap>,
    pub on_close: Option<OnClose>,
    /// No real upstream chunk (keepalive comments never count) for this long tears the
    /// connection down: emit `stall_frame` if given, stop reading upstream, close the outgoing
    /// stream cleanly, and fire `on_close(IdleTimeout)`. `None` disables this (the default).
    pub idle_timeout: Option<Duration>,
    /// Emitted once, immediately before close, when `idle_timeout` fires.
    pub stall_frame: Option<Bytes>,
    /// Terminal frame for errors before any real upstream byte was emitted.
    pub error_frame: Option<Bytes>,
}

impl StreamKeepaliveOpts {
    fn split(self) -> (Option<Tap>, CloseGuard, PipeOpts) {
        let guard = CloseGuard::new(self.on_close);
        let pipe = PipeOpts {
            tap: None,
            idle_timeout: self.idle_timeout,
            stall_frame: self.stall_frame,
            error_frame: self.error_frame,
        };
        (self.tap, guard, pipe)
    }
}

/// The subset of the options that applies to one piped upstream body.
#[derive(Default, Clone)]
pub struct PipeOpts {
    pub tap: Option<Tap>,
    pub idle_timeout: Option<Duration>,
    pub stall_frame: Option<Bytes>,
    pub error_frame: Option<Bytes>,
}

/// Fires `on_close` exactly once. The pump records the real reason before it finishes; if the
/// pump is dropped first — the client went away and the response body was dropped — the guard
/// reports `Cancel`, so callers that settle a lease or write a log row on close always run.
struct CloseGuard {
    reason: Mutex<Option<StreamCloseReason>>,
    /// Behind a `Mutex` so the guard is `Sync` and can be shared with the eager controller; a
    /// `FnOnce` is only ever taken once, in `Drop`.
    on_close: Mutex<Option<OnClose>>,
}

impl CloseGuard {
    fn new(on_close: Option<OnClose>) -> Self {
        Self { reason: Mutex::new(None), on_close: Mutex::new(on_close) }
    }
    /// The first reason recorded wins, exactly like the TypeScript `closeFired` latch.
    fn fire(&self, reason: StreamCloseReason) {
        let mut slot = self.reason.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(reason);
        }
    }
}

impl Drop for CloseGuard {
    fn drop(&mut self) {
        let reason = (*self.reason.get_mut().unwrap_or_else(|e| e.into_inner())).unwrap_or(StreamCloseReason::Cancel);
        if let Some(on_close) = self.on_close.get_mut().unwrap_or_else(|e| e.into_inner()).take() {
            on_close(reason);
        }
    }
}

fn tap(tap: &Option<Tap>, chunk: &Bytes) {
    if let Some(f) = tap {
        f(chunk);
    }
}

/// Relay an upstream body, injecting SSE comment keepalives across every silence gap — not just
/// before the first byte — for the whole stream lifetime. `opts.tap`/`opts.on_close` observe
/// the passthrough without altering it: emitted upstream bytes and their relative order are
/// byte-identical to calling this with no opts. Keepalive comments and an optional idle
/// `stall_frame` are the only bytes this function itself ever adds.
pub fn stream_with_keepalive(upstream: ByteStream, interval: Duration, opts: StreamKeepaliveOpts) -> ByteStream {
    let (outer_tap, guard, pipe) = opts.split();
    let pipe = PipeOpts { tap: outer_tap, ..pipe };
    backpressured_byte_stream(move |emitter| async move {
        let guard = guard;
        let reason = pipe_body(&emitter, upstream, interval, &pipe, None).await;
        guard.fire(reason);
        Ok(())
    })
}

/// One piped upstream body, under the keepalive + idle-timeout rules. `outer_tap` is the
/// producer-level tap that also sees every chunk (the eager transport's `opts.tap`).
async fn pipe_body(
    emitter: &Emitter,
    mut upstream: ByteStream,
    interval: Duration,
    opts: &PipeOpts,
    outer_tap: Option<&Tap>,
) -> StreamCloseReason {
    let mut emitted_upstream_bytes = false;
    // Both deadlines cover the whole stream lifetime: `keepalive_at` restarts after every byte
    // this function emits, `idle_at` only after a real upstream chunk.
    let mut keepalive_at = Instant::now() + interval;
    let mut idle_at = opts.idle_timeout.map(|d| Instant::now() + d);
    loop {
        let next = upstream.next();
        tokio::pin!(next);
        let chunk = loop {
            let idle_sleep = async {
                match idle_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                item = &mut next => break item,
                _ = tokio::time::sleep_until(keepalive_at) => {
                    if emitter.enqueue(sse_keepalive_comment()).await.is_err() {
                        return StreamCloseReason::Cancel;
                    }
                    keepalive_at = Instant::now() + interval;
                }
                _ = idle_sleep => {
                    if let Some(frame) = opts.stall_frame.clone() {
                        let _ = emitter.enqueue(frame).await;
                    }
                    // Stop reading upstream and close the outgoing stream cleanly.
                    return StreamCloseReason::IdleTimeout;
                }
            }
        };
        match chunk {
            None => return StreamCloseReason::Done,
            Some(Err(error)) => {
                // Once a real byte has flowed the stream may be mid-frame and a raw abort is
                // the only honest option. Before output, preserve the client protocol with the
                // dispatch-provided terminal frame.
                if !emitted_upstream_bytes {
                    if let Some(frame) = opts.error_frame.clone() {
                        let _ = emitter.enqueue(frame).await;
                        return StreamCloseReason::Done;
                    }
                }
                tracing::debug!(%error, "upstream stream failed mid-flight");
                return StreamCloseReason::Error;
            }
            Some(Ok(bytes)) => {
                if emitter.enqueue(bytes.clone()).await.is_err() {
                    return StreamCloseReason::Cancel;
                }
                emitted_upstream_bytes = true;
                tap(&opts.tap, &bytes);
                if let Some(outer) = outer_tap {
                    outer(&bytes);
                }
                // Re-arm for the next gap.
                keepalive_at = Instant::now() + interval;
                idle_at = opts.idle_timeout.map(|d| Instant::now() + d);
            }
        }
    }
}

/// Controller handed to [`stream_with_eager_producer`]'s `run` callback. Keepalives already
/// fire from stream start; the idle timeout arms only inside [`EagerStreamController::pipe_upstream`]
/// (docs/api.md § Eager streaming commit).
pub struct EagerStreamController {
    emitter: Emitter,
    interval: Duration,
    outer_tap: Option<Tap>,
    guard: Arc<CloseGuard>,
    closed: Arc<AtomicBool>,
    /// While a body is piping, `pipe_body` owns the keepalive cadence — the producer-level
    /// ticker stands down so a comment can never land between two halves of one upstream frame.
    piping: Arc<AtomicBool>,
}

impl EagerStreamController {
    /// The client already cancelled — stop acquire/upstream work.
    pub fn cancelled(&self) -> bool {
        self.emitter.is_cancelled()
    }

    /// The same check as a shareable closure, for the candidate walk's `cancelled` hook.
    pub fn cancel_signal(&self) -> Arc<dyn Fn() -> bool + Send + Sync> {
        let emitter = self.emitter.clone();
        Arc::new(move || emitter.is_cancelled())
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Relay an upstream body with the same keepalive re-arm plus optional idle timeout rules
    /// as [`stream_with_keepalive`]. Returns when the upstream ends, the idle timeout fires, or
    /// the client cancels. The idle timeout starts only here — never during the pre-upstream
    /// TTFB wait.
    pub async fn pipe_upstream(&self, upstream: ByteStream, opts: PipeOpts) {
        if self.is_closed() || self.cancelled() {
            drop(upstream);
            return;
        }
        self.piping.store(true, Ordering::SeqCst);
        let reason = pipe_body(&self.emitter, upstream, self.interval, &opts, self.outer_tap.as_ref()).await;
        self.piping.store(false, Ordering::SeqCst);
        self.closed.store(true, Ordering::SeqCst);
        self.guard.fire(reason);
    }

    /// Enqueue one terminal frame (an in-stream error) and close cleanly. A terminal in-stream
    /// error is a clean end from the pipe's view; callers force `error_code` from their own
    /// state when they log.
    pub async fn fail(&self, frame: Bytes) {
        if self.is_closed() {
            return;
        }
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.emitter.enqueue(frame).await;
        self.guard.fire(StreamCloseReason::Done);
    }

    /// Close cleanly with no extra frame.
    pub fn close(&self) {
        if self.is_closed() {
            return;
        }
        self.closed.store(true, Ordering::SeqCst);
        self.guard.fire(if self.cancelled() { StreamCloseReason::Cancel } else { StreamCloseReason::Done });
    }
}

/// SSE stream that commits immediately: keepalives from second 0, while `run` performs
/// acquire / failover / upstream fetch and either pipes a body or emits a terminal error frame.
/// Used for client `stream: true` (docs/api.md § Eager streaming commit).
pub fn stream_with_eager_producer<F, Fut>(run: F, interval: Duration, opts: StreamKeepaliveOpts) -> ByteStream
where
    F: FnOnce(EagerStreamController) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (outer_tap, guard, _) = opts.split();
    backpressured_byte_stream(move |emitter| async move {
        let guard = Arc::new(guard);
        let closed = Arc::new(AtomicBool::new(false));
        let piping = Arc::new(AtomicBool::new(false));
        let ctl = EagerStreamController {
            emitter: emitter.clone(),
            interval,
            outer_tap,
            guard: guard.clone(),
            closed: closed.clone(),
            piping: piping.clone(),
        };
        // Keepalive from second 0 — before any upstream body exists. The producer and the
        // ticker share one task, so a keepalive only ever fills a real silence gap: the tick
        // deadline restarts whenever the producer itself emits (`pipe_body` re-arms its own).
        let produce = run(ctl);
        tokio::pin!(produce);
        loop {
            tokio::select! {
                biased;
                () = &mut produce => break,
                _ = tokio::time::sleep(interval) => {
                    if closed.load(Ordering::SeqCst) || piping.load(Ordering::SeqCst) {
                        // The producer has finished with the stream, or `pipe_upstream` owns
                        // the cadence for now; let it return.
                        continue;
                    }
                    if emitter.enqueue(sse_keepalive_comment()).await.is_err() {
                        break;
                    }
                }
            }
        }
        if !closed.load(Ordering::SeqCst) {
            guard.fire(if emitter.is_cancelled() { StreamCloseReason::Cancel } else { StreamCloseReason::Done });
        }
        Ok::<(), io::Error>(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::mpsc;

    /// A source stream the test pushes chunks into (or closes) on demand.
    fn controllable() -> (ByteStream, mpsc::UnboundedSender<Bytes>) {
        let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
        let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx).map(Ok);
        (Box::pin(stream), tx)
    }

    /// One chunk, then silence forever — never closes.
    fn never_ending(initial: &'static str) -> ByteStream {
        let (stream, tx) = controllable();
        tx.send(Bytes::from_static(initial.as_bytes())).unwrap();
        // Leak the sender so the stream never ends.
        std::mem::forget(tx);
        stream
    }

    fn from_text(text: &'static str) -> ByteStream {
        Box::pin(futures::stream::once(async move { Ok(Bytes::from_static(text.as_bytes())) }))
    }

    async fn collect(mut stream: ByteStream) -> String {
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("no stream error"));
        }
        String::from_utf8(out).expect("utf8")
    }

    fn reasons() -> (Arc<Mutex<Vec<StreamCloseReason>>>, OnClose) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        (seen, Box::new(move |reason| sink.lock().unwrap().push(reason)))
    }

    fn taps() -> (Arc<Mutex<Vec<String>>>, Tap) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        (seen, Arc::new(move |chunk: &Bytes| sink.lock().unwrap().push(String::from_utf8_lossy(chunk).into_owned())))
    }

    #[tokio::test]
    async fn relays_real_bytes_unmodified_and_in_order() {
        let (stream, tx) = controllable();
        tx.send(Bytes::from_static(b"hello ")).unwrap();
        tx.send(Bytes::from_static(b"world")).unwrap();
        drop(tx);
        let out = stream_with_keepalive(stream, DEFAULT_KEEPALIVE_INTERVAL, StreamKeepaliveOpts::default());
        assert_eq!(collect(out).await, "hello world");
    }

    #[tokio::test]
    async fn re_arms_the_keepalive_after_a_real_chunk() {
        let (stream, tx) = controllable();
        let out = stream_with_keepalive(stream, Duration::from_millis(10), StreamKeepaliveOpts::default());
        tx.send(Bytes::from_static(b"hello")).unwrap();
        let collected = tokio::spawn(collect(out));
        tokio::time::sleep(Duration::from_millis(120)).await;
        drop(tx);
        let text = collected.await.unwrap();
        assert!(text.contains("hello"));
        // The old behavior stopped keepalives forever after the first byte (0 here).
        assert!(text.matches(": keepalive").count() >= 2, "{text}");
    }

    #[tokio::test]
    async fn the_idle_timeout_emits_the_stall_frame_and_reports_idle_timeout() {
        let (seen, on_close) = reasons();
        let out = stream_with_keepalive(
            never_ending("hello"),
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts {
                idle_timeout: Some(Duration::from_millis(20)),
                stall_frame: Some(Bytes::from_static(b"STALL\n")),
                on_close: Some(on_close),
                ..Default::default()
            },
        );
        let text = collect(out).await;
        assert!(text.contains("hello"), "{text}");
        assert!(text.contains("STALL"), "{text}");
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::IdleTimeout]);
    }

    #[tokio::test]
    async fn the_idle_timeout_never_fires_while_chunks_keep_arriving() {
        let (seen, on_close) = reasons();
        let (stream, tx) = controllable();
        let out = stream_with_keepalive(
            stream,
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts {
                idle_timeout: Some(Duration::from_millis(60)),
                stall_frame: Some(Bytes::from_static(b"STALL\n")),
                on_close: Some(on_close),
                ..Default::default()
            },
        );
        let collected = tokio::spawn(collect(out));
        for chunk in ["a", "b", "c"] {
            tx.send(Bytes::copy_from_slice(chunk.as_bytes())).unwrap();
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        drop(tx);
        assert_eq!(collected.await.unwrap(), "abc");
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::Done]);
    }

    #[tokio::test]
    async fn the_idle_timeout_is_disabled_by_default() {
        let (seen, on_close) = reasons();
        let (stream, tx) = controllable();
        let out = stream_with_keepalive(
            stream,
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts { on_close: Some(on_close), ..Default::default() },
        );
        let collected = tokio::spawn(collect(out));
        tx.send(Bytes::from_static(b"hello")).unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        drop(tx);
        assert_eq!(collected.await.unwrap(), "hello");
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::Done]);
    }

    #[tokio::test]
    async fn a_clean_upstream_end_reports_done() {
        let (seen, on_close) = reasons();
        let out = stream_with_keepalive(
            from_text("hi"),
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts { on_close: Some(on_close), ..Default::default() },
        );
        collect(out).await;
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::Done]);
    }

    #[tokio::test]
    async fn dropping_the_outgoing_stream_reports_cancel_exactly_once() {
        let (seen, on_close) = reasons();
        let mut out = stream_with_keepalive(
            never_ending("hi"),
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts { on_close: Some(on_close), ..Default::default() },
        );
        assert_eq!(out.next().await.unwrap().unwrap(), Bytes::from_static(b"hi"));
        drop(out);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::Cancel]);
    }

    #[tokio::test]
    async fn on_close_fires_once_even_when_a_natural_close_and_a_drop_race() {
        let (seen, on_close) = reasons();
        let mut out = stream_with_keepalive(
            from_text("hi"),
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts { on_close: Some(on_close), ..Default::default() },
        );
        assert!(out.next().await.is_some());
        assert!(out.next().await.is_none());
        drop(out);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::Done]);
    }

    #[tokio::test]
    async fn an_upstream_error_reports_error_once_bytes_have_flowed() {
        let (seen, on_close) = reasons();
        let source = futures::stream::iter(vec![
            Ok(Bytes::from_static(b"a")),
            Err(io::Error::other("boom")),
        ]);
        let out = stream_with_keepalive(
            Box::pin(source),
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts { on_close: Some(on_close), ..Default::default() },
        );
        assert_eq!(collect(out).await, "a");
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::Error]);
    }

    #[tokio::test]
    async fn an_upstream_error_before_any_byte_emits_the_terminal_frame() {
        let (seen, on_close) = reasons();
        let source = futures::stream::iter(vec![Err(io::Error::other("boom"))]);
        let out = stream_with_keepalive(
            Box::pin(source),
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts {
                on_close: Some(on_close),
                error_frame: Some(Bytes::from_static(b"ERR\n")),
                ..Default::default()
            },
        );
        assert_eq!(collect(out).await, "ERR\n");
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::Done]);
    }

    #[tokio::test]
    async fn the_tap_sees_every_real_chunk_and_never_a_keepalive() {
        let (seen, tap) = taps();
        let (stream, tx) = controllable();
        let out = stream_with_keepalive(
            stream,
            Duration::from_millis(10),
            StreamKeepaliveOpts { tap: Some(tap), ..Default::default() },
        );
        let collected = tokio::spawn(collect(out));
        tx.send(Bytes::from_static(b"chunk-1")).unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        tx.send(Bytes::from_static(b"chunk-2")).unwrap();
        drop(tx);
        collected.await.unwrap();
        assert_eq!(*seen.lock().unwrap(), vec!["chunk-1".to_string(), "chunk-2".to_string()]);
    }

    #[tokio::test]
    async fn keepalive_does_not_drain_upstream_ahead_of_client_demand() {
        let served = Arc::new(AtomicUsize::new(0));
        let counter = served.clone();
        let source = futures::stream::repeat_with(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Bytes::from_static(b"chunk\n"))
        });
        let mut out = stream_with_keepalive(
            Box::pin(source),
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts::default(),
        );
        out.next().await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(served.load(Ordering::SeqCst) < 6, "{}", served.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn eager_keepalives_fire_before_pipe_upstream() {
        let (seen, on_close) = reasons();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let out = stream_with_eager_producer(
            move |ctl| async move {
                let _ = gate_rx.await;
                ctl.close();
            },
            Duration::from_millis(15),
            StreamKeepaliveOpts { on_close: Some(on_close), ..Default::default() },
        );
        let collected = tokio::spawn(collect(out));
        tokio::time::sleep(Duration::from_millis(60)).await;
        let _ = gate_tx.send(());
        let text = collected.await.unwrap();
        assert!(text.contains(": keepalive"), "{text}");
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::Done]);
    }

    #[tokio::test]
    async fn eager_pipe_upstream_relays_bytes_unmodified() {
        let (stream, tx) = controllable();
        tx.send(Bytes::from_static(b"hello ")).unwrap();
        tx.send(Bytes::from_static(b"world")).unwrap();
        drop(tx);
        let out = stream_with_eager_producer(
            move |ctl| async move {
                ctl.pipe_upstream(stream, PipeOpts::default()).await;
            },
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts::default(),
        );
        assert_eq!(collect(out).await, "hello world");
    }

    #[tokio::test]
    async fn eager_fail_emits_the_frame_and_closes_cleanly() {
        let (seen, on_close) = reasons();
        let out = stream_with_eager_producer(
            move |ctl| async move {
                ctl.fail(Bytes::from_static(b"ERR\n")).await;
            },
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts { on_close: Some(on_close), ..Default::default() },
        );
        assert_eq!(collect(out).await, "ERR\n");
        // A terminal in-stream error is a clean end from the pipe's view.
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::Done]);
    }

    #[tokio::test]
    async fn eager_cancel_during_the_pre_upstream_wait_reports_cancel() {
        let (seen, on_close) = reasons();
        let out = stream_with_eager_producer(
            move |_ctl| async move {
                std::future::pending::<()>().await;
            },
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts { on_close: Some(on_close), ..Default::default() },
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(out);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::Cancel]);
    }

    #[tokio::test]
    async fn the_eager_idle_timeout_arms_only_after_pipe_upstream_starts() {
        let (seen, on_close) = reasons();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let out = stream_with_eager_producer(
            move |ctl| async move {
                // Spend longer than the idle window before piping — it must not stall yet.
                let _ = gate_rx.await;
                ctl.pipe_upstream(
                    never_ending("first"),
                    PipeOpts {
                        idle_timeout: Some(Duration::from_millis(25)),
                        stall_frame: Some(Bytes::from_static(b"STALL\n")),
                        ..Default::default()
                    },
                )
                .await;
            },
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts { on_close: Some(on_close), ..Default::default() },
        );
        let collected = tokio::spawn(collect(out));
        tokio::time::sleep(Duration::from_millis(80)).await;
        let _ = gate_tx.send(());
        let text = collected.await.unwrap();
        assert!(text.contains("first"), "{text}");
        assert!(text.contains("STALL"), "{text}");
        assert_eq!(*seen.lock().unwrap(), vec![StreamCloseReason::IdleTimeout]);
    }

    #[tokio::test]
    async fn eager_pipe_upstream_does_not_drain_upstream_ahead_of_client_demand() {
        let served = Arc::new(AtomicUsize::new(0));
        let counter = served.clone();
        let source = futures::stream::repeat_with(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Bytes::from_static(b"chunk\n"))
        });
        let mut out = stream_with_eager_producer(
            move |ctl| async move {
                ctl.pipe_upstream(Box::pin(source), PipeOpts::default()).await;
            },
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts::default(),
        );
        out.next().await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(served.load(Ordering::SeqCst) < 6, "{}", served.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_cancelled_producer_sees_cancelled_and_never_pipes() {
        let flag = Arc::new(AtomicBool::new(false));
        let observed = flag.clone();
        let out = stream_with_eager_producer(
            move |ctl| async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                observed.store(ctl.cancelled(), Ordering::SeqCst);
            },
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts::default(),
        );
        drop(out);
        tokio::time::sleep(Duration::from_millis(80)).await;
        // The pump is aborted with the stream, so the producer never even gets that far — what
        // matters is that nothing is left running against a gone client.
        assert!(!flag.load(Ordering::SeqCst));
    }
}
