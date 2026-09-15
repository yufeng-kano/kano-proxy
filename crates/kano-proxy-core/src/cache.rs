//! In-process TTL cache replacing the Worker's KV `CACHE` namespace (docs/rust-server.md
//! § Storage). Keys keep their KV shapes (`models:v1:<user>:<provider>`, `pricing:litellm:v1`,
//! `changelog:v1`, replay caches) so the documented lifetimes carry over. Values are
//! bounded by the callers (replay caches cap at 256 KiB).

use std::time::Duration;

use bytes::Bytes;
use moka::future::Cache as Moka;

#[derive(Clone)]
struct Entry {
    ttl: Duration,
    value: Bytes,
}

#[derive(Clone)]
pub struct Cache {
    inner: Moka<String, Entry>,
    /// Distinguishes cache instances, so per-process memos keyed on it never leak between
    /// separately built apps (production has one cache; tests build many).
    id: u64,
}

static NEXT_CACHE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Cache {
    pub fn new() -> Self {
        let inner = Moka::builder()
            .max_capacity(256 * 1024 * 1024)
            .weigher(|k: &String, e: &Entry| (k.len() + e.value.len()).min(u32::MAX as usize) as u32)
            .expire_after(TtlExpiry)
            .build();
        Self { inner, id: NEXT_CACHE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub async fn get(&self, key: &str) -> Option<Bytes> {
        self.inner.get(key).await.map(|e| e.value)
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        let b = self.get(key).await?;
        serde_json::from_slice(&b).ok()
    }

    pub async fn get_text(&self, key: &str) -> Option<String> {
        let b = self.get(key).await?;
        String::from_utf8(b.to_vec()).ok()
    }

    /// `ttl` mirrors KV `expirationTtl` (seconds).
    pub async fn put(&self, key: &str, value: Bytes, ttl: Duration) {
        self.inner.insert(key.to_string(), Entry { ttl, value }).await;
    }

    pub async fn put_json<T: serde::Serialize>(&self, key: &str, value: &T, ttl: Duration) {
        let b = serde_json::to_vec(value).expect("cache value serializes");
        self.put(key, Bytes::from(b), ttl).await;
    }

    pub async fn put_text(&self, key: &str, value: &str, ttl: Duration) {
        self.put(key, Bytes::copy_from_slice(value.as_bytes()), ttl).await;
    }

    pub async fn delete(&self, key: &str) {
        self.inner.invalidate(key).await;
    }
}

struct TtlExpiry;

impl moka::Expiry<String, Entry> for TtlExpiry {
    fn expire_after_create(&self, _key: &String, entry: &Entry, _now: std::time::Instant) -> Option<Duration> {
        Some(entry.ttl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_and_expiry() {
        let c = Cache::new();
        c.put_text("k", "v", Duration::from_millis(50)).await;
        assert_eq!(c.get_text("k").await.as_deref(), Some("v"));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(c.get_text("k").await, None);
    }
}
