# Logging

## Allowed

- Auth events: login success/fail (user id / email), logout
- Request metadata: user_id, api_key_id, provider, model, account_id, status, latency_ms, prompt_tokens, completion_tokens, cache_read_input_tokens, cache_creation_input_tokens, cost (estimated USD, [pricing.md](./pricing.md)), error_code, upstream_status (the last upstream HTTP status observed — kept because `status_code` hides it: eager streams always log `200` and the synthesized `503` masks the bench-status that emptied the pool, which is exactly what made the 2026-08-14 pause undiagnosable; see [database.md](./database.md))
- Pool: bench, unpause, promote, remove (ids only)

## Error codes

`request_logs.error_code` (nullable TEXT), current full set:

| Code | Meaning |
|------|---------|
| `no_upstream_account` | No account **bound** for the resolved provider (pool empty, every account removed, or every account ineligible for the requested model — [providers.md](./providers.md) § Routing module "Candidates") — 400, retrying can never help |
| `upstream_unavailable` | Accounts exist but none usable — every bound account benched at request start, or the failover loop exhausted its attempts (503, `Retry-After` = seconds until earliest bench expiry when known) |
| `upstream_error` | Upstream returned a non-2xx after retries/failover, or every account got benched and there was a prior upstream response to fall back to |
| `invalid_model` | Authenticated request; `model` did not resolve to a builtin provider or one of the caller's own custom provider slugs (400, pre-dispatch) |
| `loop_detected` | Degenerate tool-call loop guard tripped on a grok/codex/antigravity/custom-openai conversion request (400, pre-dispatch) — see [api.md](./api.md) |
| `upstream_stall` | Streaming response: no upstream chunk for 120s — idle-timeout close, logged unconditionally regardless of whether a completion signal was already seen |
| `client_abort` | Streaming response: the client cancelled before the upstream reported a completion signal |
| `incomplete_stream` | Streaming response: the upstream connection ended — cleanly or with a transport error — before a completion signal ever arrived |
| `spend_limit_exceeded` | Authenticated request refused pre-dispatch because the key's spend-limit window is exhausted (429, [pricing.md](./pricing.md)) |
| `count_tokens_stub` | `count_tokens` answered locally with the sentinel `{"input_tokens": 0}` (grok, custom-openai without `count_tokens_url`, or a codex relay count that failed/was unconfigured) — 200, no upstream call, no account touched ([api.md](./api.md) § count_tokens) |
| `NULL` | Success, or a streamed response that reached its documented completion signal before closing |

## Pre-dispatch logging policy

A request that fails before any upstream call is logged only when the caller **authenticated successfully** — an invalid/missing API key never writes a row (these are public endpoints; logging every scanner/bot hit would flood `request_logs` with nothing actionable). Once the API key is valid, every pre-dispatch failure gets exactly one row via `waitUntil`, same as a real dispatch:

- Invalid `model` (`/v1/messages`, `/v1/messages/count_tokens`, `/chat/completions`): `error_code: "invalid_model"`; `provider` = the text before the first `/` in the raw model string, or `"unknown"` when there was no `/`; `model` = the raw string truncated to 200 chars.
- No usable upstream account on the first acquire attempt, with no prior upstream response to fall back to (`/v1/messages` native passthrough — parity with the `/openai/v1` path, which already logged this case): `error_code: "no_upstream_account"`.
- Loop guard trip (see [api.md](./api.md)): `error_code: "loop_detected"`.

## Streaming rows

A streamed request's row is written once, deferred to stream close (`waitUntil`) — see "Token usage capture" below. Two independent fields describe how it ended:

- **Eager commit** (`stream: true`): the Worker returns `200` + SSE headers **before** acquire / upstream, so `status_code` is always `200` for these rows — a failure discovered after commit (no account, pool exhausted, upstream 4xx/5xx, idle stall, client abort) cannot change the HTTP status. The failure mode lives in `error_code` (and the in-stream error frame the client already saw). See [api.md](./api.md) "Eager streaming commit".
- **Legacy attach** (only when a non-eager path still waits for upstream headers then wraps an event-stream body): `status_code` is whatever those upstream headers said (typically `200`).
- `error_code` for a stream close:
  - **Forced terminal failures** set during the in-stream failover loop take priority: `no_upstream_account`, `upstream_unavailable`, `upstream_error` (same meanings as the Error codes table).
  - Otherwise, from how the pipe closed and whether the sniffer saw the upstream's documented completion signal (Anthropic `message_stop`; OpenAI `[DONE]` or a chunk carrying a non-null `finish_reason`): `upstream_stall` (idle-timeout close, regardless of completeness), `client_abort` (client cancelled first, and no completion signal had been seen yet), or `incomplete_stream` (the connection just ended — clean EOF or a transport error — with no completion signal). A stream that did reach its completion signal keeps `error_code: NULL`.
- `latency_ms` for an eager stream is measured from request start until upstream headers are obtained (TTFB into the pipe), or until a terminal in-stream failure / cancel if that happens first — not until the last token. That keeps long prefill visible in the dashboard without conflating it with full generation time.

### Why pre-commit timeouts left no row

Before eager commit, a client that abandoned the connection while the Worker was still `await`ing upstream response headers tore down the invocation with **no** `logRequest` (every log site sat after that await). Those incidents produced client-side timeouts and empty D1. Eager commit + stream `cancel` → `client_abort` closes that gap.

## Forbidden by default

- Full prompts, completions, tool arguments/results bodies
- Upstream access/refresh tokens
- Client API key plaintext
- OAuth codes / cookies

Retention: swept by a daily Worker Cron Trigger — see below.

## Temporary diagnostics

A short-lived `console.*` probe is allowed when production is the only place a question can be answered for free, provided it logs no forbidden value above, is scoped to the failing branch, and **names the release that removes it**. Anything still in the tree past that release is a bug.

Read one with `wrangler tail` or Dashboard → Workers → Logs while the relevant page refreshes.

**None active.** The last was `[antigravity] no GOOGLE_ONE_AI credit entry` (v3.11.2, removed in v3.11.3): it established that a Google AI Pro account returns the credit entry with no `creditAmount` field at all, recorded in [providers.md](./providers.md) § Antigravity. One caution learned from it — the `paidTier` subtree also carried the account's email inside `upgradeSubscriptionUri`. Scope a probe to the fields you need, not a whole subtree, and assume an unexamined payload holds an identifier you did not plan to log.

## Retention sweep

A daily Cron Trigger (`[triggers] crons` in every Wrangler config — committed `wrangler.toml`, `wrangler.production.example.toml`, the CI generator, and the operator's gitignored `wrangler.production.toml`; `scheduled` handler in `apps/api/src/index.ts`) deletes:

- `request_logs` rows older than **90 days** (default; override with the optional `REQUEST_LOG_RETENTION_DAYS` var, a positive integer of days — invalid values fall back to 90). Deletes run in bounded batches (id-subquery `IN (SELECT … LIMIT n)` loop with a per-run batch cap) so a large backlog never produces one long-running statement; steady state is roughly one day of rows per run.
- Expired `sessions` rows (`expires_at` in the past — `loadSessionUser` already rejects them; this just removes dead rows).
- Expired `oauth_login_states` rows (`expires_at` in the past).
- Expired `cli_login_requests` rows (`expires_at` in the past — [cli.md](./cli.md) § Device auth).

The sweep is idempotent and safe to run any time (locally: `wrangler dev --test-scheduled`, then `GET /__scheduled?cron=...`). A sweep failure affects nothing but cleanup — it never touches live request handling.

## Token usage capture

`request_logs` token fields are captured per path (normalized semantics in [database.md](./database.md)):

| Path | Non-stream | Stream |
|------|-----------|--------|
| `/anthropic` native passthrough (`claude-code`, custom `format=anthropic`) | response JSON `usage` | passthrough parse of `message_start` / `message_delta` usage while piping |
| `/openai/v1` → `claude-code` / custom `format=anthropic`; codex on either surface | converted response `usage` (includes cached details — see [api.md](./api.md)) | converter-attached final-chunk `usage` |
| `antigravity` (both surfaces) | converted `usage` built from Gemini `usageMetadata` — `thoughtsTokenCount` is added into the output count and also reported as `reasoning_tokens` / kept out of Anthropic `input_tokens`; `cachedContentTokenCount` becomes `cached_tokens` / `cache_read_input_tokens` | converter-attached final chunk (`/openai/v1`) or `message_delta.usage` (`/anthropic`), carrying the **whole** usage object rather than output tokens alone — Gemini reports its input counts on the same frames, after `message_start` already went out |
| `/openai/v1/responses` native path (all-codex candidates) | collected `response.completed` → `response.usage` (`input_tokens`, `output_tokens`, `input_tokens_details.cached_tokens`) | passthrough parse of the `response.completed` event while piping; the OpenAI sniffer recognizes that event alongside chat chunks, and it doubles as the completion signal |
| `/openai/v1/responses` conversion path | as the target's `/openai/v1` row above — the Responses ↔ Chat conversion sits outside dispatch, so capture sees the same chat stream / JSON it always did | same |
| `grok` / custom `format=openai` | response JSON `usage` (`prompt_tokens_details.cached_tokens` and `cache_write_tokens` when upstream reports them) | **best-effort:** captured when the upstream stream carries a `usage` chunk. custom-openai always sets `stream_options.include_usage: true` (stream and non-stream); grok sets it on stream. If the upstream still omits the chunk, token fields stay `NULL` |

Rules:

- Capture never rewrites or buffers the client stream: streamed bodies are inspected chunk-by-chunk as they pass through the existing keepalive pipe, holding only a bounded partial-line carry. On carry overflow or parse failure, capture is abandoned but the stream keeps flowing — a capture problem degrades to `NULL` token fields, never a failed request.
- Exactly **one** `request_logs` row per client request, including the `/anthropic` → OpenAI-convert path.
- `count_tokens` requests log a row like any other request but never carry token fields (their response is an estimate, not consumption).
- OpenAI-shaped `usage.completion_tokens` is already the total completion count inclusive of reasoning tokens (with `completion_tokens_details.reasoning_tokens` reported as a detail breakdown). It is stored directly as `request_logs.completion_tokens` and mapped to Anthropic `output_tokens` on conversion paths without adding `reasoning_tokens` a second time.

The Overview page aggregates these rows via `GET /api/usage/summary`, and the Logs page lists them per request via `GET /api/logs` (see [auth.md](./auth.md), [admin-ui.md](./admin-ui.md)).
