# Logging

## Allowed

- Auth events: login success/fail (user id / email), logout
- Request metadata: user_id, api_key_id, api_key_name and account_label (the names as they were at write time, so a later deletion leaves the row readable — [database.md](./database.md)), provider, model, group_name, account_id, status_code, latency_ms, prompt_tokens, completion_tokens, cache_read_input_tokens, cache_creation_input_tokens, cost (estimated USD, [pricing.md](./pricing.md)), error_code, started_at, upstream_status (the last upstream HTTP status observed — kept because `status_code` hides it: eager streams always log `200` and the synthesized `503` masks the bench-status that emptied the pool; see [database.md](./database.md))
- Pool: bench, unpause, reorder, remove (ids only)

## Error codes

`request_logs.error_code` (nullable TEXT), current full set:

| Code | Meaning |
|------|---------|
| `no_upstream_account` | No account **bound** for the resolved provider (pool empty, every account removed, or every account ineligible for the requested model — [providers.md](./providers.md) § Routing module "Candidates") — 400, retrying can never help |
| `upstream_unavailable` | Accounts exist but none usable — every bound account benched at request start, or the failover loop exhausted its attempts (503, `Retry-After` = seconds until earliest bench expiry when known) |
| `upstream_error` | A streamed request whose upstream answered a non-bench non-2xx after failover; an adapter call that failed outright (non-stream `502`); an audio transcription whose upstream answered non-2xx or whose body failed mid-read. A non-stream Chat/Messages upstream error passed through keeps `error_code` `NULL` — its `status_code` is the upstream status |
| `invalid_model` | Authenticated request; `model` did not resolve to a builtin provider or one of the caller's own custom provider slugs, or is not one of the group's model names on a group endpoint (400, pre-dispatch) |
| `group_not_found` | Authenticated request to a `/g/<slug>/…` endpoint whose slug is not one of the caller's groups (404, pre-dispatch) |
| `loop_detected` | Degenerate tool-call loop guard tripped on a grok/codex/antigravity/custom-openai conversion request (400, pre-dispatch) — see [api.md](./api.md) § Degenerate tool-call loop guard |
| `unsupported_field` | `/openai/v1/responses` request needing server-side state or carrying an unconvertible part (400, pre-dispatch — [api.md](./api.md) § `POST /openai/v1/responses`) |
| `unsupported_modality` | Audio input, or an audio transcription, for a target whose upstream wire has no audio (400, pre-dispatch — [api.md](./api.md) § Audio input) |
| `unsupported_audio_format` | Audio part with no recognized `format` and no data-URL mime, on a converting target (400, pre-dispatch) |
| `invalid_request` | Audio transcription without its `file` part (400, pre-dispatch) |
| `upstream_stall` | Streaming response: no upstream chunk for 120s — idle-timeout close, logged unconditionally regardless of whether a completion signal was already seen |
| `client_abort` | Streaming response (or an audio transcription body): the client cancelled before the upstream reported a completion signal |
| `incomplete_stream` | Streaming response: the upstream connection ended — cleanly or with a transport error, including a body that failed before its first byte — before a completion signal ever arrived |
| `spend_limit_exceeded` | Authenticated request refused pre-dispatch because the key's spend-limit window is exhausted (429, [pricing.md](./pricing.md)) |
| `count_tokens_stub` | `count_tokens` answered locally with the sentinel `{"input_tokens": 0}` (grok, custom-openai without `count_tokens_url`, or a codex body the local tokenizer could not count) — 200, no upstream call, no account touched ([api.md](./api.md) § `POST /anthropic/v1/messages/count_tokens`) |
| `NULL` | Success, or a streamed response that reached its documented completion signal before closing |

## Pre-dispatch logging policy

A request that fails before any upstream call is logged only when the caller **authenticated successfully** — an invalid/missing API key never writes a row (these are public endpoints; logging every scanner/bot hit would flood `request_logs` with nothing actionable). Once the API key is valid, every pre-dispatch failure gets exactly one row, written off the request path, same as a real dispatch:

- Invalid `model` or unknown group slug (every LLM route that takes a `model`): `error_code: "invalid_model"` / `"group_not_found"`; `provider` = the text before the first `/` in the raw model string (the whole string when it has none), or `"unknown"` when it is empty; `model` = the raw string truncated to 200 chars.
- No account bound for the resolved provider: `error_code: "no_upstream_account"`, the requested target with no `account_id`.
- Loop guard trip ([api.md](./api.md) § Degenerate tool-call loop guard): `error_code: "loop_detected"`.
- An unsupported Responses field, audio modality or audio format, or a transcription without `file`: the matching code from the table above, logged against the highest-priority resolved target.
- Spend limit reached: `error_code: "spend_limit_exceeded"`, `provider` `"unknown"`, no model, no token fields, no cost ([pricing.md](./pricing.md) § Per-key spend limits).

Invalid JSON and an invalid reasoning effort are answered `400` without a row.

## Streaming rows

A streamed request's row is written once, deferred to stream close — see "Token usage capture" below. Two independent fields describe how it ended:

- **Eager commit** (`stream: true`): the server returns `200` + SSE headers **before** acquire / upstream, so `status_code` is always `200` for these rows — a failure discovered after commit (no account, pool exhausted, upstream 4xx/5xx, idle stall, client abort) cannot change the HTTP status. The failure mode lives in `error_code` (and the in-stream error frame the client already saw). See [api.md](./api.md) "Eager streaming commit".
- **Legacy attach** (a non-stream request whose upstream answered with an event-stream body anyway: the path waits for upstream headers, then wraps that body): `status_code` is whatever those upstream headers said (typically `200`).
- `provider` / `model` / `account_id` on an eager row describe the **last candidate actually attempted** (canonical `provider/model`, expanded from a group alias when one was used), on both surfaces; a row that failed before any attempt (`no_upstream_account`, pool `unavailable`) carries the requested target with no `account_id`.
- `error_code` for a stream close:
  - **Forced terminal failures** set during the in-stream failover loop take priority: `no_upstream_account`, `upstream_unavailable`, `upstream_error` (same meanings as the Error codes table).
  - Otherwise, from how the pipe closed and whether the sniffer saw the upstream's documented completion signal (Anthropic `message_stop`; OpenAI `[DONE]` or a chunk carrying a non-null `finish_reason`; Responses `response.completed` / `response.incomplete`): `upstream_stall` (idle-timeout close, regardless of completeness), `client_abort` (client cancelled first, and no completion signal had been seen yet), or `incomplete_stream` (the connection just ended — clean EOF or a transport error — with no completion signal). A stream that did reach its completion signal keeps `error_code: NULL`.
- `latency_ms` for an eager stream is measured from request start until upstream headers are obtained (TTFB into the pipe), or until a terminal in-stream failure / cancel if that happens first — not until the last token. That keeps long prefill visible in the dashboard without conflating it with full generation time.

Eager commit is also what gives a client that gives up before upstream headers arrive a row: its stream `cancel` logs `client_abort`. A log site after the header wait would never run for it.

## Forbidden by default

- Full prompts, completions, tool arguments/results bodies
- Upstream access/refresh tokens
- Client API key plaintext
- OAuth codes / cookies

Retention: swept by a daily scheduled task — see below.

## Temporary diagnostics

A short-lived diagnostic log line is allowed when production is the only place a question can be answered for free, provided it logs no forbidden value above, is scoped to the failing branch, and **names the release that removes it**. Anything still in the tree past that release is a bug.

Read one in the server's own log output while the relevant page refreshes.

**None active.** Scope a probe to the fields you need, not a whole subtree, and assume an unexamined payload holds an identifier you did not plan to log: Antigravity's `paidTier` subtree, for one, carries the account's email inside `upgradeSubscriptionUri`.

## Retention sweep

A scheduled task inside the server runs daily at 03:17 UTC (then refreshes the price table when it is missing or stale, [pricing.md](./pricing.md)) and deletes:

- `request_logs` rows older than **90 days** (default; override with the optional `REQUEST_LOG_RETENTION_DAYS` var, a positive integer of days — invalid values fall back to 90). Deletes run in batches of 2000 (an id-subquery `IN (SELECT … LIMIT n)` loop), at most 40 per run, so a large backlog never produces one long-running statement and drains over several days; steady state is roughly one day of rows per run.
- Expired `sessions` rows (`expires_at` in the past — `load_session_user` already rejects them; this just removes dead rows).
- Expired `oauth_login_states` rows (`expires_at` in the past).
- Expired `cli_login_requests` rows (`expires_at` in the past — [cli.md](./cli.md) § Device auth).

The sweep is idempotent and safe to run at any time. A sweep failure affects nothing but cleanup — it never touches live requests.

## Token usage capture

`request_logs` token fields are captured per path (normalized semantics in [database.md](./database.md)):

| Path | Non-stream | Stream |
|------|-----------|--------|
| `/anthropic` native passthrough (`claude-code`, custom `format=anthropic`) | response JSON `usage` | passthrough parse of `message_start` / `message_delta` usage while piping |
| `/openai/v1` → `claude-code` / custom `format=anthropic`; codex on either surface | converted response `usage` (includes cached details — see [api.md](./api.md)) | converter-attached final-chunk `usage` |
| `antigravity` (both surfaces) | converted `usage` built from Gemini `usageMetadata` — `thoughtsTokenCount` is added into the output count and also reported as `reasoning_tokens` / kept out of Anthropic `input_tokens`; `cachedContentTokenCount` becomes `cached_tokens` / `cache_read_input_tokens` | converter-attached final chunk (`/openai/v1`) or `message_delta.usage` (`/anthropic`), carrying the **whole** usage object rather than output tokens alone — Gemini reports its input counts on the same frames, after `message_start` already went out |
| `/openai/v1/responses` native path (all-codex candidates) | collected `response.completed` → `response.usage` (`input_tokens`, `output_tokens`, `input_tokens_details.cached_tokens`) | passthrough parse of the `response.completed` event while piping; the OpenAI sniffer recognizes that event alongside chat chunks, and it doubles as the completion signal. On a real Codex CLI turn that event is routinely longer than the sniffer's 256 KiB line cap (the ChatGPT backend echoes the request into it), so it is never buffered: the sniffer identifies it by its `event:` line or leading `"type"` and scans its small `usage` object out of the passing bytes — see the rules below |
| `/openai/v1/responses` conversion path | as the target's `/openai/v1` row above — the Responses ↔ Chat conversion sits outside dispatch, so capture sees the same chat stream / JSON as a Chat Completions request | same |
| `grok` / custom `format=openai` | response JSON `usage` (`prompt_tokens_details.cached_tokens` and `cache_write_tokens` when upstream reports them) | **best-effort:** captured when the upstream stream carries a `usage` chunk. custom-openai always sets `stream_options.include_usage: true` (stream and non-stream); grok sets it on stream. If the upstream still omits the chunk, token fields stay `NULL` |
| `/openai/v1/audio/transcriptions` (custom `format=openai`) | the JSON body's `usage` when the upstream reports one (text, SRT and VTT bodies carry none) | an event-stream body is read like a Chat stream |

Rules:

- Capture never rewrites or buffers the client stream (the separate `usage.cost` annotation on `/openai/v1/chat/completions` is the one rewrite, [api.md](./api.md) § Cost in usage; capture reads the annotated bytes and ignores `cost`): streamed bodies are inspected chunk-by-chunk as they pass through the existing keepalive pipe, holding only a bounded partial-line carry (256 KiB). A line that outgrows the carry is **skipped, not fatal** — capture resumes on the next line. The one over-long line that matters is a Responses `response.completed` / `response.incomplete` event on the native codex path: recognized by its `event:` line or its leading `"type"`, its `usage` object (a few hundred bytes; scanned through a 4 KiB window, validated as an object carrying numeric token counts so a tool schema with a `usage` property cannot be mistaken for it) is picked out of the bytes as they pass, and the line still counts as the completion signal. A parse failure still degrades to `NULL` token fields, never a failed request.
- Exactly **one** `request_logs` row per client request, including the `/anthropic` → OpenAI-convert path.
- `count_tokens` requests log a row like any other request but never carry token fields (their response is an estimate, not consumption).
- OpenAI-shaped `usage.completion_tokens` is already the total completion count inclusive of reasoning tokens (with `completion_tokens_details.reasoning_tokens` reported as a detail breakdown). It is stored directly as `request_logs.completion_tokens` and mapped to Anthropic `output_tokens` on conversion paths without adding `reasoning_tokens` a second time.

The Overview page aggregates these rows via `GET /api/usage/summary`, and the Logs page lists them per request via `GET /api/logs`, counts today's models and aliases via `GET /api/logs/facets`, and downloads them via `GET /api/logs/export` — the same columns, so an export carries no content either (see [auth.md](./auth.md), [admin-ui.md](./admin-ui.md)).
