# Database (D1)

Migrations live in `apps/api/migrations/` and are applied only via Wrangler.

## Tables

### `users`

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | uuid |
| google_sub | TEXT UNIQUE | OIDC subject |
| email | TEXT | |
| name | TEXT | |
| picture_url | TEXT | nullable |
| created_at | TEXT | ISO |
| updated_at | TEXT | ISO |

### `sessions`

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | random |
| user_id | TEXT FK | |
| expires_at | TEXT | |
| created_at | TEXT | |

### `api_keys`

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | |
| name | TEXT | user label |
| key_prefix | TEXT | display |
| key_hash | TEXT UNIQUE | |
| created_at | TEXT | |
| last_used_at | TEXT | nullable |
| spend_limit | REAL | nullable; USD ceiling per window, `NULL` = unlimited ([pricing.md](./pricing.md)) |
| spend_limit_interval | TEXT | `daily` \| `weekly` \| `monthly` \| `total`; `NOT NULL DEFAULT 'monthly'` |
| spend_limit_include_oauth | INTEGER | 0/1, `NOT NULL DEFAULT 1`; whether builtin-provider (subscription OAuth) traffic counts toward the limit |

Spend-limit columns added in `0004_api_key_spend_limits.sql`.

### `upstream_accounts`

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | |
| provider | TEXT | builtin `claude-code` \| `codex` \| `grok`, **or** a custom provider's `slug` (see `custom_providers` below) |
| external_account_id | TEXT | nullable; upstream account id when known (e.g. codex's ChatGPT account id) |
| label | TEXT | email or display, **synced from upstream** on every accounts read — not user-editable |
| custom_label | TEXT | nullable; the operator's own name for this account. Wins over `label` for display and is **never** overwritten by the upstream sync (`0005_account_custom_label.sql`) |
| priority | INTEGER | higher = preferred; promote bumps |
| encrypted_payload | TEXT | AES-GCM blob: tokens + provider fields. For a custom provider this is just `{access_token: <api key>}` |
| account_meta_json | TEXT | email, plan, non-secret. For a custom provider: `{key_mask: "sk-abc…f3a2"}` (see [providers.md](./providers.md)) |
| usage_snapshot_json | TEXT | nullable; last successful usage read — `{windows, error, stale, edgeBlocked}` (`0006_account_usage_cache.sql`) |
| usage_fetched_at | TEXT | nullable; when `usage_snapshot_json` was written. Drives the 60s server-side TTL |
| usage_fetching_at | TEXT | nullable; lock holder's timestamp while an upstream fetch is in flight (`NULL` = free) |
| created_at | TEXT | |
| updated_at | TEXT | |

`label` and `custom_label` are two different jobs and must not be merged: `label` is a cache of upstream identity (the accounts read overwrites it whenever the upstream email/display name changes), so a rename written there survives only until the next poll. `custom_label` is user intent — set by `PATCH /api/providers/:provider/accounts/:id`, cleared by sending `null`/`""`, and read first by the display-name resolver.

**There is no persisted `status` column** — `0001_init.sql` never created one. "Active / standby / benched / unusable" (or, for a custom provider, the simpler "active" / "benched") is computed at read time from `priority` order plus the KV bench state (`pool/bench.ts`, `BENCH` namespace), never stored. (An earlier revision of this doc incorrectly listed a `status` column; fixed 2026-08-02.)

Unique optional: `(user_id, provider, external_account_id)` when known.

### `custom_providers`

User-defined custom upstream providers (BYO endpoint + API key — see [providers.md](./providers.md)). Its API key(s) are ordinary `upstream_accounts` rows with `provider = slug`; this table only holds the provider-level config, and has no FK from `upstream_accounts` back to it (deleting a custom provider deletes its account rows at the application layer, not via `ON DELETE CASCADE`).

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | `ON DELETE CASCADE` |
| slug | TEXT | immutable after creation; unique per user |
| name | TEXT | display name |
| format | TEXT | `openai` \| `anthropic`; immutable after creation |
| base_url | TEXT | validated (https, no credentials/query/fragment, not localhost/private/loopback/own-host) and trailing-slash-stripped on save |
| models_mode | TEXT | `auto` \| `manual`; `NOT NULL DEFAULT 'auto'` |
| manual_models_json | TEXT | nullable; JSON array of upstream model id strings |
| sort_order | INTEGER | display order within the user's list, ascending; `NOT NULL DEFAULT 0` (`0007_custom_provider_sort_order.sql`) |
| created_at | TEXT | |
| updated_at | TEXT | |

`sort_order` is **display only** — it never affects routing, pooling, or failover (a custom provider is selected by slug, not by list position; within-provider key priority stays in `upstream_accounts.priority`). Backfilled by `created_at ASC` so existing lists keep their current visual order. Reads sort `ORDER BY sort_order ASC, created_at ASC`, so ties and a all-zero legacy table degrade to the old behavior. Writes renumber the user's full list to a dense `0..n-1` sequence in one transaction rather than patching single rows; a create appends at the end.

`UNIQUE(user_id, slug)`. No `status` column here either — same computed-from-KV-bench convention as `upstream_accounts`, over that provider's account row(s).

### `usage_snapshots` — **deprecated, never used**

Created by `0001_init.sql` as an "optional cache", never read or written by any code. The 60s usage cache it anticipated now lives in the `upstream_accounts` columns above; a separate table would cost one extra write per refresh and a second query per read, for nothing. Left in place rather than dropped — an empty table is free, and a `DROP TABLE` migration is a schema change with no benefit. **Do not build on it.**

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| account_id | TEXT FK | |
| fetched_at | TEXT | |
| payload_json | TEXT | windows, stale flag |
| stale | INTEGER | 0/1 |

### `request_logs`

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT | |
| api_key_id | TEXT | |
| provider | TEXT | |
| model | TEXT | |
| account_id | TEXT | nullable |
| status_code | INTEGER | |
| latency_ms | INTEGER | |
| prompt_tokens | INTEGER | nullable; **total** input tokens, including cached reads and cache writes |
| completion_tokens | INTEGER | nullable |
| cache_read_input_tokens | INTEGER | nullable; input tokens served from upstream prompt cache |
| cache_creation_input_tokens | INTEGER | nullable; cache-write tokens, from Anthropic `cache_creation_input_tokens`, OpenAI-compatible `prompt_tokens_details.cache_write_tokens`, or the proxy's conversion extension |
| cost | REAL | nullable; estimated USD cost computed at write time from the LiteLLM price table ([pricing.md](./pricing.md)); `NULL` = unpriced/unknown, never 0-as-guess |
| error_code | TEXT | nullable |
| created_at | TEXT | |

**No message content, no prompts, no completions.**

Token semantics (normalized across providers — capture matrix in [logging.md](./logging.md)):

- `prompt_tokens` is always the **total** input count. Anthropic-shaped usage reports `input_tokens` *excluding* cache reads/writes, so the logged value is `input_tokens + cache_read_input_tokens + cache_creation_input_tokens`; OpenAI-shaped `prompt_tokens` already includes cached tokens and is stored as-is.
- `cache_read_input_tokens` maps from Anthropic `cache_read_input_tokens`, OpenAI `prompt_tokens_details.cached_tokens`, or Responses `input_tokens_details.cached_tokens`. `cache_creation_input_tokens` maps from Anthropic `cache_creation_input_tokens`, OpenAI-compatible `prompt_tokens_details.cache_write_tokens`, or the proxy's `cache_creation_input_tokens` conversion extension.
- `NULL` means *unreported*, not zero: when an Anthropic-shaped `usage` is present, absent cache fields default to `0` (the API defines them); OpenAI-shaped usage stores `NULL` unless the corresponding `prompt_tokens_details` member (or the `cache_creation_input_tokens` extension) was actually present. Cache-rate aggregation divides only over rows where `cache_read_input_tokens IS NOT NULL`.
- Streamed requests write their row when the stream ends (`waitUntil`), so token fields can be populated; a client that disconnects mid-stream still gets a row with whatever usage was seen by then.

Cache columns added in `0003_request_log_cache_tokens.sql`; `cost` and the spend-limit index `request_logs_api_key_created_idx` on `(api_key_id, created_at)` added in `0004_api_key_spend_limits.sql`. Dashboard range queries are covered by the existing `request_logs_user_created_idx` on `(user_id, created_at)` from `0001_init.sql`.

Rows past the retention window (default 90 days) are deleted by the daily cron sweep, which also purges expired `sessions` and `oauth_login_states` rows — see [logging.md](./logging.md).

### `oauth_login_states`

Pending provider or Google login state (PKCE verifier, expiry). Short-lived; may use KV instead.

## KV namespaces

Cloudflare titles: `kano-proxy-bench`, `kano-proxy-cache`. Bindings in Worker code stay short:

| Binding | Cloudflare title | Use |
|---------|------------------|-----|
| `BENCH` | `kano-proxy-bench` | `userId:provider:accountId` → bench-until epoch (TTL ~300s) |
| `CACHE` | `kano-proxy-cache` | usage snapshots, models list cache |

D1 database name: **`kano-proxy`** (Worker script name is also `kano-proxy`).

## Migrations

```bash
cd apps/api
npx wrangler d1 migrations create kano-proxy --local "init"
npx wrangler d1 migrations apply kano-proxy --local
npx wrangler d1 migrations apply kano-proxy --remote
```
