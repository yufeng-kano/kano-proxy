# Logging

## Allowed

- Auth events: login success/fail (user id / email), logout
- Request metadata: user_id, api_key_id, provider, model, account_id, status, latency_ms, prompt_tokens, completion_tokens, cache_read_input_tokens, cache_creation_input_tokens, error_code
- Pool: bench, promote, remove (ids only)

## Forbidden by default

- Full prompts, completions, tool arguments/results bodies
- Upstream access/refresh tokens
- Client API key plaintext
- OAuth codes / cookies

Retention: swept by a daily Worker Cron Trigger — see below.

## Retention sweep

A daily Cron Trigger (`[triggers] crons` in every Wrangler config — committed `wrangler.toml`, `wrangler.production.example.toml`, the CI generator, and the operator's gitignored `wrangler.production.toml`; `scheduled` handler in `apps/api/src/index.ts`) deletes:

- `request_logs` rows older than **90 days** (default; override with the optional `REQUEST_LOG_RETENTION_DAYS` var, a positive integer of days — invalid values fall back to 90). Deletes run in bounded batches (id-subquery `IN (SELECT … LIMIT n)` loop with a per-run batch cap) so a large backlog never produces one long-running statement; steady state is roughly one day of rows per run.
- Expired `sessions` rows (`expires_at` in the past — `loadSessionUser` already rejects them; this just removes dead rows).
- Expired `oauth_login_states` rows (`expires_at` in the past).

The sweep is idempotent and safe to run any time (locally: `wrangler dev --test-scheduled`, then `GET /__scheduled?cron=...`). A sweep failure affects nothing but cleanup — it never touches live request handling.

## Token usage capture

`request_logs` token fields are captured per path (normalized semantics in [database.md](./database.md)):

| Path | Non-stream | Stream |
|------|-----------|--------|
| `/anthropic` native passthrough (`claude-code`, custom `format=anthropic`) | response JSON `usage` | passthrough parse of `message_start` / `message_delta` usage while piping |
| `/openai/v1` → `claude-code` / custom `format=anthropic`; codex on either surface | converted response `usage` (includes cached details — see [api.md](./api.md)) | converter-attached final-chunk `usage` |
| `grok` / custom `format=openai` | response JSON `usage` (`prompt_tokens_details.cached_tokens` when upstream reports it) | **best-effort:** captured only when the upstream stream carries a `usage` chunk (e.g. client sent `stream_options.include_usage`); otherwise token fields stay `NULL` |

Rules:

- Capture never rewrites or buffers the client stream: streamed bodies are inspected chunk-by-chunk as they pass through the existing keepalive pipe, holding only a bounded partial-line carry. On carry overflow or parse failure, capture is abandoned but the stream keeps flowing — a capture problem degrades to `NULL` token fields, never a failed request.
- Exactly **one** `request_logs` row per client request, including the `/anthropic` → OpenAI-convert path.
- `count_tokens` requests log a row like any other request but never carry token fields (their response is an estimate, not consumption).

The Dashboard admin page aggregates these rows via `GET /api/usage/summary` (see [auth.md](./auth.md), [admin-ui.md](./admin-ui.md)).
