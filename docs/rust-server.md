# Rust server

The server is a Rust binary and a PostgreSQL database, run with docker compose on a machine the operator controls. This document is the rationale the stack rule requires for the primary language and runtime (operator decision, 2026-09-15).

## Why

- A serverless runtime such as Cloudflare Workers shapes the product around its limits: 128 MB of isolate memory (too little for an in-process tokenizer), CPU-time ceilings, no interactive database transactions, `waitUntil` lifetimes for stream settlement, and platform-injected egress headers that make chatgpt.com answer `403` ([providers.md](./providers.md) § The chatgpt.com wall).
- One Rust process on ordinary Linux has none of them: real database transactions, in-process caches, an in-process WebSocket registry for CLI tunnels, direct upstream egress, and no per-request compute ceiling.
- The CLI is Rust too, so the tunnel protocol has one definition per side instead of two implementations.
- Self-hosting is `docker compose up` with a `.env` file.

Layout and module boundaries: [project-structure.md](./project-structure.md). An edition sets its request policy, pool extension, service name and terms date on `AppState::builder`, then calls `build_router(AppState, Extensions)` with its extra routes and the core paths it serves itself (`Extensions::routes`, `Extensions::shadowed_paths`). Nothing is registered globally, so two routers built from the same source never see each other's.

## Storage

PostgreSQL. `migrations/0001_core_baseline.sql` keeps the original column shapes: TEXT ids, ISO-8601 text timestamps and 0/1 integer flags on the baseline's tables; columns added later use native types where that reads straight into Rust (`BOOLEAN`). The runner, history table and immutability rule are in [database.md](./database.md) § Migrations.

Caches, the tunnel and the daily sweep are in-process: TTL caches for model catalogs, reasoning replay and the price table; a registry keyed by `cli_providers.id` preserving [cli.md](./cli.md) frames, bounds and close codes; a scheduled task for retention and price refresh. Egress is direct, so `count_tokens` runs its tokenizer here.

## Configuration

Everything is environment variables, listed in `.env.example` and documented in [deployment.md](./deployment.md) § Configuration. The process itself reads `DATABASE_URL`, `LISTEN_ADDR` (default `0.0.0.0:8787`) and `WEB_DIST_DIR` (a built SPA to serve; unset serves the API only).

## Persisted formats

Stored rows and issued credentials use fixed byte layouts, which the crypto module must keep: API keys `sk-kano-proxy-` + base64url(24 bytes) looked up by hex SHA-256; session cookie `kano-proxy_session=<id>.<hex HMAC-SHA256>`; credentials `base64(iv12 ‖ AES-256-GCM ciphertext)` with the key derivation in [auth.md](./auth.md) § Encryption; CLI access tokens `base64url(JSON).hexHMAC`. Unit tests pin these against fixtures generated with an independent implementation. The tunnel protocol is `proto = 1`.

## Verification

Commands, database-backed tests and the cost rule: [testing.md](./testing.md).

## Module map and shared seams

The crate is one module per concern, in snake_case; `docs/` is the contract for each one, named in its doc comment.

Shared seams are owned by the crate root. A module needing a change to one reports it rather than changing it in place, so the seam stays consistent for every module:

| Seam | Rust |
|---|---|
| Configuration | `CoreConfig` (`config.rs`) |
| Storage | `AppState::pool()` (`sqlx` Postgres), `db::*` query modules, `db::test_support` for tests |
| Outbound HTTP | `AppState::transport()` (`upstream::UpstreamTransport`; `MockTransport` in tests) |
| Cache | `AppState::cache()` (`cache::Cache`, TTL per put) |
| Background work | `tokio::spawn` |
| Errors | `http::ApiError` (surface-aware envelopes, `x-should-retry`); `http::errors::admin_error` for the management routes' `{"error": …}` body |
| Identity | `extensions::ApiKeyIdentity` in request extensions; the `auth::session::SessionUser` / `MaybeSessionUser` extractors |
| Extensions | `extensions::{RequestPolicy, Extensions}`, `pool::PoolExtension`, `app::AppStateBuilder` |
| Adapters | `providers::ProviderAdapter` (`async_trait`), `AcquiredAccount`, `ChatCompletionRequest` |
| Time | `app::now_ms()` (epoch ms), `ids::now_iso()` (ISO-8601 with milliseconds, `Z`) |
| Responses | `axum::response::Response` with `Body::from_stream` for SSE; `bytes::Bytes` for bounded bodies |

Conventions:

- JSON is `serde_json::Value` with `preserve_order`, so object key order stays stable for the prefix hashes and golden tests that depend on it. Typed structs are used where a shape is stable (rows, credentials, usage).
- Streams are never buffered whole: converters operate line by line (`proxy::sse_lines`) and dispatch pipes with bounded backpressure, as [api.md](./api.md) § Streaming requires. Bounded reads (JSON error bodies, non-stream responses) go through `UpstreamResponse::bytes()`.
- Route handlers return a `Response`, building errors from `ApiError` or `admin_error`; storage errors map to `500 internal_error`, never a panic. Logging goes through `tracing` and never includes prompts, completions, tokens or secrets ([logging.md](./logging.md)).
- Tests live beside what they test: pure modules as `#[cfg(test)] mod tests`, HTTP surfaces through `axum::Router` + `tower::ServiceExt::oneshot`, upstreams through `MockTransport`, storage through `db::test_support::test_pool()` (a fresh database per test on `KANO_TEST_DATABASE_URL`, migrated with the core migrations). No test reaches a real upstream.
