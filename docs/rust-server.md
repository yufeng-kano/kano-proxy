# Rust server

The server is a Rust binary and a PostgreSQL database, run with docker compose on a machine the operator controls. Operator decision, 2026-09-15; it replaced a TypeScript Cloudflare Worker, and this document is the rationale the stack rule requires for a primary language and runtime change. The edition it replaced is kept on the `serverless` branch.

## Why

- The Worker runtime shaped the product around its limits: 128 MB of isolate memory (the tokenizer had to live in a separate Cloud Run service), CPU-time ceilings, no interactive database transactions, `waitUntil` lifetimes for stream settlement, and Cloudflare-injected egress headers that made chatgpt.com answer `403` — the sole reason an egress relay existed at all.
- One Rust process on ordinary Linux removes all of them: real database transactions, in-process caches, an in-process WebSocket registry for CLI tunnels, direct upstream egress, and no per-request compute ceiling.
- The CLI was already Rust, so the tunnel protocol has one definition per side instead of two implementations.
- Self-hosting is `docker compose up` with a `.env` file instead of a Cloudflare account, Wrangler, D1 and KV.

## Layout

```text
apps/api/                        # the server's own Rust workspace
  Cargo.toml                        # workspace over crates/*
  crates/kano-proxy-core/           # the library an edition composes
    src/config.rs                   # environment → CoreConfig
    src/db/                         # Postgres pool, migration runner, per-table queries
    src/crypto/                     # keys, credential encryption, sessions, CLI tokens
    src/app.rs                      # build_router(state, Extensions), SPA fallback, serve()
    src/extensions.rs               # composition-time extension points (no global registry)
    migrations/0001_core_baseline.sql
  crates/kano-proxy-api/         # the standalone binary
```

Editions call `build_router(AppState, Extensions)` and add their routes, request policy and pool extension there. Nothing is registered globally, so two routers built from the same source never see each other's.

## Storage

PostgreSQL. `migrations/0001_core_baseline.sql` is the schema as it stood at the end of the Cloudflare era, expressed once: the same tables, columns, TEXT ids, ISO-8601 text timestamps and 0/1 integer flags, so an exported database imported row for row and every existing id survived. Applied migrations are immutable; later changes are new files. The runner records names in `core_migrations`, one transaction per file; editions keep their own table and apply after the core, as [database.md](./database.md) requires.

Caches, the tunnel and the daily sweep are in-process: TTL caches for model catalogs, reasoning replay and the price table; a registry keyed by `cli_providers.id` preserving [cli.md](./cli.md) frames, bounds and close codes; a scheduled task for retention and price refresh. Egress is direct, so no relay sits in the path and `count_tokens` runs its tokenizer here.

## Configuration

Everything is environment variables, listed in `.env.example` and documented in [deployment.md](./deployment.md). Bindings are `DATABASE_URL`, `LISTEN_ADDR` (default `0.0.0.0:8787`) and `WEB_DIST_DIR` (a built SPA to serve; unset serves the API only). Secrets live in the operator's gitignored `.env`; tracked examples use placeholders.

## Compatibility

Existing data and clients kept working without user action, so the crypto module reproduces the original byte layouts exactly: API keys `sk-kano-proxy-` + base64url(24 bytes) looked up by hex SHA-256; session cookie `kano-proxy_session=<id>.<hex HMAC-SHA256>`; credentials `base64(iv12 ‖ AES-256-GCM ciphertext)` with the documented key derivation; CLI access tokens `base64url(JSON).hexHMAC`. Unit tests pin these against fixtures generated with an independent implementation. The tunnel protocol stays `proto = 1`.

## Verification

`cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` from `apps/api`; the CLI keeps its own `cargo test` inside `apps/cli`. Database-backed tests need `KANO_TEST_DATABASE_URL` (a throwaway Postgres; CI starts one) and skip without it; each test gets a fresh database and stale ones are swept. No test sends real upstream traffic, as the cost rule requires.

The port completed on 2026-09-16 with 1295 tests passing and clippy clean. The released CLI binary (1.0.0) was paired against this server with the `--no-tui` flow, registered a local OpenAI-format provider, connected with `proto 1`, reported its models, and served `/openai/v1/models`, non-stream and streaming chat completions and an Anthropic-surface conversion through the tunnel with correct `request_logs` rows; the local target was a stub server, so no real upstream was called.

## Module map and shared seams

The crate is one module per concern, in snake_case; `docs/` is the contract for each one, named in its doc comment.

Shared seams are owned by the crate root. A module needing a change to one reports it rather than changing it in place, so the seam stays consistent for every module:

| Seam | Rust |
|---|---|---|
| Configuration | `CoreConfig` (`config.rs`) |
| Storage | `AppState::pool()` (`sqlx` Postgres), `db::*` query modules, `db::test_support` for tests |
| Outbound HTTP | `AppState::transport()` (`upstream::UpstreamTransport`; `MockTransport` in tests) |
| Cache | `AppState::cache()` (`cache::Cache`, TTL per put) |
| Background work | `tokio::spawn` |
| Errors | `http::ApiError` (surface-aware envelopes, `x-should-retry`, `Retry-After`) |
| Identity | `extensions::ApiKeyIdentity` in request extensions; session user likewise |
| Extensions | `extensions::{RequestPolicy, Extensions}`, `pool::PoolExtension` |
| Adapters | `providers::ProviderAdapter` (`async_trait`), `AcquiredAccount`, `ChatCompletionRequest` |
| Time | `app::now_ms()` (epoch ms), `ids::now_iso()` (JS `toISOString`) |
| Responses | `axum::response::Response` with `Body::from_stream` for SSE; `bytes::Bytes` for bounded bodies |

Conventions:

- JSON is `serde_json::Value` with `preserve_order`, so object key order stays stable for the prefix hashes and golden tests that depend on it. Typed structs are used where a shape is stable (rows, credentials, usage).
- Streams are never buffered whole: converters operate line by line (`proxy::sse_lines`) and dispatch pipes with bounded backpressure, as `docs/api.md` § Streaming requires. Bounded reads (JSON error bodies, non-stream responses) go through `UpstreamResponse::bytes()`.
- Route handlers return `Result<Response, ApiError>`; storage errors map to `500 internal_error`, never a panic. Logging goes through `tracing` and never includes prompts, completions, tokens or secrets (`docs/logging.md`).
- Tests live beside what they test: pure modules as `#[cfg(test)] mod tests`, HTTP surfaces through `axum::Router` + `tower::ServiceExt::oneshot`, upstreams through `MockTransport`, storage through `db::test_support::test_pool()` (a fresh database per test on `KANO_TEST_DATABASE_URL`, migrated with the baseline). No test reaches a real upstream.
