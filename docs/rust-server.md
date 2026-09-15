# Rust server

Operator decision (2026-09-15): the proxy moves from Cloudflare Workers to a Rust server run with docker compose. This is the docs rationale the stack rule requires for a new primary language and non-Cloudflare runtime. The `rust` branch carries the port; `main` and the Worker remain authoritative until the private edition cuts over ([cloud-edition.md](./cloud-edition.md)).

## Why

- Worker limits shaped the product around workarounds: 128 MB isolate memory (the tokenizer lives in the Cloud Run relay), CPU-time ceilings, no interactive D1 transactions, `waitUntil` lifetimes for stream settlement, and Cloudflare-injected egress headers that make chatgpt.com answer 403, the sole reason [codex-relay.md](./codex-relay.md) exists.
- One Rust process on ordinary Linux removes all of them: real database transactions, in-process caches, an in-process WebSocket registry for CLI tunnels, direct upstream egress, no per-request compute ceiling.
- The CLI is already Rust; its wire protocol (`apps/cli/src/protocol.rs`) becomes a shared crate instead of a second implementation.
- Self-hosting becomes `docker compose up` with a `.env` file instead of a Cloudflare account, Wrangler, D1 and KV.

## Layout

```text
Cargo.toml                      # workspace: crates/kano-proxy-core, apps/server (apps/cli stays standalone)
crates/kano-proxy-core/         # library an edition composes
  src/config.rs                 # environment → CoreConfig (same names as the Worker Env)
  src/db.rs                     # Postgres pool, migration runner, core_migrations
  src/crypto/                   # WebCrypto-compatible keys, credential encryption, sessions, CLI tokens
  src/app.rs                    # build_router(state, Extensions), SPA fallback, serve()
  src/extensions.rs             # composition-time extension points (no global registry)
  migrations/0001_core_baseline.sql
apps/server/                    # standalone binary: core + built web app
```

Editions call `build_router(AppState, Extensions)`; the private edition adds its routes, request policy and pool extension there, as `createApplication` does today. Route groups arrive phase by phase; until a group is ported the TypeScript Worker under `apps/api` remains the reference and its docs remain the contract.

## Storage

Postgres replaces D1. `migrations/0001_core_baseline.sql` is the final D1 shape after `apps/api/migrations/0001..0017` expressed once: same tables, columns, TEXT ids, ISO-8601 text timestamps and 0/1 integer flags, so an exported D1 database imports row for row and existing ids survive. Applied migrations are immutable; later changes are new files appended to `db::CORE_MIGRATIONS`. The runner records names in `core_migrations` inside one transaction per file; editions keep their own table and apply after the core, as [database.md](./database.md) requires for D1.

KV, Durable Objects and cron map to in-process state: TTL caches for model catalogs, reasoning replay and the price table; a tunnel registry keyed by `cli_providers.id` preserving [cli.md](./cli.md) frames, bounds and close codes; a scheduled task for retention and price refresh. The Codex relay is not needed when egress is direct.

## Configuration

Environment variables keep the Worker names (`APP_URL`, `GOOGLE_REDIRECT_URI`, `GOOGLE_CLIENT_ID/SECRET`, `SESSION_SECRET`, `TOKEN_ENCRYPTION_KEY`, `CLI_TOKEN_SECRET`, `ANTIGRAVITY_*`, `REQUEST_LOG_RETENTION_DAYS`, `UPSTREAM_FIRST_BYTE_TIMEOUT_MS`, `GITHUB_REPO`, `GITHUB_TOKEN`, optional provider client ids). Bindings become `DATABASE_URL`, `LISTEN_ADDR` (default `0.0.0.0:8787`) and `WEB_DIST_DIR` (the built SPA with `/docs/` inside; unset serves the API only). Secrets stay in the operator's gitignored `.env`; tracked examples use placeholders.

## Compatibility

Existing data and clients must keep working without user action, so the crypto module reproduces the WebCrypto byte layouts exactly: API keys `sk-kano-proxy-` + base64url(24 bytes) looked up by hex SHA-256; session cookie `kano-proxy_session=<id>.<hex HMAC-SHA256>`; credentials `base64(iv12 ‖ AES-256-GCM ciphertext)` with the documented key derivation; CLI access tokens `base64url(JSON).hexHMAC`. Unit tests pin these against fixtures generated with Node's WebCrypto running the TypeScript algorithms (`crates/kano-proxy-core/src/crypto/*/tests`). The tunnel protocol stays `proto = 1`.

## Verification

`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --release` at the repository root (the CLI keeps `cargo test` inside `apps/cli`). Database-backed tests need `KANO_TEST_DATABASE_URL` (a throwaway Postgres; CI starts one) and skip without it; each test gets a fresh database and stale ones are swept. Route ports carry their TypeScript test cases across with stubbed upstreams; no real upstream traffic, as the cost rule requires.

Status 2026-09-16: every module of `apps/api/src` is ported and 1295 tests pass (three consecutive full runs, clippy clean). The released CLI binary (1.0.0, built from `apps/cli`) was paired against the Rust server with the `--no-tui` flow, registered a local OpenAI-format provider, connected with `proto 1`, reported its models, and served `/openai/v1/models`, non-stream and streaming chat completions and an Anthropic-surface conversion through the tunnel with correct `request_logs` rows; the local target was a stub server, so no real upstream was called.

## Module map and porting conventions

The crate mirrors `apps/api/src` one module per TypeScript file, same names in snake_case (`do/` becomes `tunnel/`). Every module's doc comment names its TypeScript source and the docs section that owns the behavior; the TypeScript file and its tests are the specification for the port, and the docs remain the contract. A port is complete when the vitest cases for that file exist as Rust tests and pass.

Shared seams, owned by the crate root and not changed by module ports:

| Seam | Rust | Replaces |
|---|---|---|
| Configuration | `CoreConfig` (`config.rs`) | Worker `Env` vars/secrets |
| Storage | `AppState::pool()` (`sqlx` Postgres), `db::*` query modules, `db::test_support` for tests | D1, `env.DB` |
| Outbound HTTP | `AppState::transport()` (`upstream::UpstreamTransport`; `MockTransport` in tests) | `fetch`, test `fetch` stubs |
| Cache | `AppState::cache()` (`cache::Cache`, TTL per put) | KV `CACHE` |
| Background work | `tokio::spawn` | `ctx.waitUntil` |
| Errors | `http::ApiError` (surface-aware envelopes, `x-should-retry`, `Retry-After`) | per-route `c.json({error…})` |
| Identity | `extensions::ApiKeyIdentity` in request extensions; session user likewise | `c.get("user")`, `apiKeyUserId` |
| Extensions | `extensions::{RequestPolicy, Extensions}`, `pool::PoolExtension` | `ApplicationOptions` |
| Adapters | `providers::ProviderAdapter` (`async_trait`), `AcquiredAccount`, `ChatCompletionRequest` | `providers/types.ts` |
| Time | `app::now_ms()` (epoch ms), `ids::now_iso()` (JS `toISOString`) | `Date.now()`, `nowIso()` |
| Responses | `axum::response::Response` with `Body::from_stream` for SSE; `bytes::Bytes` for bounded bodies | `Response`, `ReadableStream` |

Conventions:

- JSON is `serde_json::Value` with `preserve_order`, so object key order matches the JavaScript objects the tests and prefix hashes depend on. Typed structs are used where a shape is stable (rows, credentials, usage).
- Streams are never buffered whole: converters operate line by line (`proxy::sse_lines`) and dispatch pipes with bounded backpressure, as `docs/api.md` § Streaming requires. Bounded reads (JSON error bodies, non-stream responses) go through `UpstreamResponse::bytes()`.
- Route handlers return `Result<Response, ApiError>`; storage errors map to `500 internal_error`, never a panic. Logging goes through `tracing` and never includes prompts, completions, tokens or secrets (`docs/logging.md`).
- Tests port the vitest cases for the same file: pure modules as `#[cfg(test)] mod tests`, HTTP surfaces through `axum::Router` + `tower::ServiceExt::oneshot`, upstreams through `MockTransport`, storage through `db::test_support::test_pool()` (a fresh database per test on `KANO_TEST_DATABASE_URL`, migrated with the baseline). No test reaches a real upstream.
- Module ports touch only their own files. A needed change to a shared seam is reported, not made in place, so the seam stays consistent for every module.
