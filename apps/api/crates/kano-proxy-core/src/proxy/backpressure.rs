//! Streaming backpressure (docs/api.md § Streaming).
//!
//! Adapts an imperative converter ("pump") to a response body without an unbounded downstream
//! queue: a bounded `tokio::sync::mpsc` channel plus a task running the pump.
//! `Emitter::enqueue` parks once [`OUTPUT_QUEUE_FRAMES`] frames are outstanding, so the pump
//! cannot run ahead of the client, and dropping the returned stream aborts the pump, which
//! drops the upstream body. Nothing is ever buffered whole.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::stream::Stream;
use tokio::sync::mpsc;

/// Frames the converter may run ahead of the client: one in the channel plus the one a parked
/// `send` holds.
pub const OUTPUT_QUEUE_FRAMES: usize = 1;

/// The pump's end of the bounded queue. Every method parks while the client has no demand.
#[derive(Clone)]
pub struct Emitter {
    tx: mpsc::Sender<Result<Bytes, io::Error>>,
}

impl Emitter {
    /// `Err` means the client is gone; pumps propagate it with `?` and stop.
    pub async fn enqueue(&self, value: Bytes) -> Result<(), io::Error> {
        self.tx.send(Ok(value)).await.map_err(|_| cancelled())
    }

    pub async fn enqueue_str(&self, value: &str) -> Result<(), io::Error> {
        self.enqueue(Bytes::copy_from_slice(value.as_bytes())).await
    }

    /// Whether the client is still attached; a pump may stop early instead of converting more.
    pub fn is_cancelled(&self) -> bool {
        self.tx.is_closed()
    }
}

/// The client is gone. Never reaches the client (nothing is reading), so the pump just unwinds.
fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "Stream cancelled")
}

/// What `pump` emits, as a stream; dropping it aborts the pump, releasing the upstream body.
struct BackpressuredStream {
    rx: mpsc::Receiver<Result<Bytes, io::Error>>,
    pump: tokio::task::JoinHandle<()>,
}

impl Drop for BackpressuredStream {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

impl Stream for BackpressuredStream {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().rx.poll_recv(cx)
    }
}

/// Runs `pump` against a bounded queue and streams what it emits. A pump error becomes the
/// stream's terminal `Err`, so the client sees the failure instead of a truncated, seemingly
/// complete stream.
pub fn backpressured_byte_stream<F, Fut>(pump: F) -> crate::upstream::transport::ByteStream
where
    F: FnOnce(Emitter) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), io::Error>> + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(OUTPUT_QUEUE_FRAMES);
    let emitter = Emitter { tx: tx.clone() };
    let handle = tokio::spawn(async move {
        if let Err(err) = pump(emitter).await {
            if err.kind() != io::ErrorKind::BrokenPipe {
                let _ = tx.send(Err(err)).await;
            }
        }
    });
    Box::pin(BackpressuredStream { rx, pump: handle })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn relays_frames_in_order_and_closes() {
        let mut out = backpressured_byte_stream(|emitter| async move {
            emitter.enqueue_str("a").await?;
            emitter.enqueue_str("b").await?;
            Ok(())
        });
        let mut seen = Vec::new();
        while let Some(chunk) = out.next().await {
            seen.push(String::from_utf8(chunk.unwrap().to_vec()).unwrap());
        }
        assert_eq!(seen, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn a_pump_error_terminates_the_stream() {
        let mut out = backpressured_byte_stream(|emitter| async move {
            emitter.enqueue_str("a").await?;
            Err(io::Error::other("boom"))
        });
        assert_eq!(out.next().await.unwrap().unwrap(), Bytes::from_static(b"a"));
        assert_eq!(out.next().await.unwrap().unwrap_err().to_string(), "boom");
        assert!(out.next().await.is_none());
    }

    #[tokio::test]
    async fn does_not_drain_upstream_ahead_of_client_demand() {
        let served = Arc::new(AtomicUsize::new(0));
        let counter = served.clone();
        let mut out = backpressured_byte_stream(move |emitter| async move {
            loop {
                counter.fetch_add(1, Ordering::SeqCst);
                emitter.enqueue_str("chunk\n").await?;
            }
        });
        out.next().await.unwrap().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        // One delivered, one queued, one parked in `send` — never the whole upstream.
        assert!(served.load(Ordering::SeqCst) < 6, "{}", served.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn dropping_the_stream_cancels_the_pump() {
        let running = Arc::new(AtomicUsize::new(0));
        let counter = running.clone();
        let mut out = backpressured_byte_stream(move |emitter| async move {
            loop {
                counter.fetch_add(1, Ordering::SeqCst);
                emitter.enqueue_str("x").await?;
            }
        });
        out.next().await.unwrap().unwrap();
        drop(out);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let after = running.load(Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(after, running.load(Ordering::SeqCst), "pump kept running after cancel");
    }
}
