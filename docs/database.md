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
| provider | TEXT | builtin `claude-code` \| `codex` \| `grok` \| `antigravity`, **or** a custom provider's `slug` (see `custom_providers` below) |
| external_account_id | TEXT | nullable; upstream account id when known (e.g. codex's ChatGPT account id) |
| label | TEXT | email or display, **synced from upstream** on every accounts read — not user-editable |
| custom_label | TEXT | nullable; the operator's own name for this account. Wins over `label` for display and is **never** overwritten by the upstream sync (`0005_account_custom_label.sql`) |
| priority | INTEGER | higher = preferred; promote bumps |
| encrypted_payload | TEXT | AES-GCM blob: tokens + provider fields. For a custom provider this is just `{access_token: <api key>}` |
| account_meta_json | TEXT | email, plan, non-secret. For a custom provider: `{key_mask: "sk-abc…f3a2"}` (see [providers.md](./providers.md)) |
| usage_snapshot_json | TEXT | nullable; last successful usage read — `{windows, error, stale, edgeBlocked}` (`0006_account_usage_cache.sql`) |
| usage_fetched_at | TEXT | nullable; when `usage_snapshot_json` was written. Drives the 2 min server-side TTL |
| usage_fetching_at | TEXT | nullable; lock holder's timestamp while an upstream fetch is in flight (`NULL` = free) |
| bench_until | TEXT | nullable; ISO time until which this account is benched (`0011_bench_and_refresh_state.sql`). `NULL` or past = not benched — expired values are compared at read, never proactively deleted. Writes are **monotonic**: a bench write only lands when it extends (`bench_until IS NULL OR bench_until < new`), so a shorter concurrent penalty can never truncate a longer one. Replaces the KV `BENCH` namespace ([providers.md](./providers.md) § Routing module) |
| bench_reason | TEXT | nullable; content-free cause of the current bench — the upstream status as text (`"429"`, `"524"`, …) or `"refresh_failed"`. Written with `bench_until`, nulled by unpause |
| refreshing_at | TEXT | nullable; OAuth refresh single-flight lock, same CAS pattern as `usage_fetching_at` (30s breakable) — see [providers.md](./providers.md) § OAuth refresh single-flight |
| edge_strikes | INTEGER | `NOT NULL DEFAULT 0`; consecutive upstream edge-timeout (520/522/524) strikes. Incremented atomically on each edge-timeout; the 3rd within 10 minutes benches 30s and resets to 0 ([providers.md](./providers.md) § Penalties). Never written on success |
| edge_strike_at | TEXT | nullable; time of the last edge-timeout strike. A value older than 10 minutes stales the count — the next increment restarts at 1 |
| created_at | TEXT | |
| updated_at | TEXT | |

`label` and `custom_label` are two different jobs and must not be merged: `label` is a cache of upstream identity (the accounts read overwrites it whenever the upstream email/display name changes), so a rename written there survives only until the next poll. `custom_label` is user intent — set by `PATCH /api/providers/:provider/accounts/:id`, cleared by sending `null`/`""`, and read first by the display-name resolver.

**There is no persisted `status` column** — `0001_init.sql` never created one. "Active / standby / limited / benched / unusable" (or, for a custom provider, the simpler "active" / "benched") is computed at read time from `priority` order, the bench columns above (`pool/bench.ts`) and the stored `usage_snapshot_json` windows (`routing/facts.ts`), never stored. (An earlier revision of this doc incorrectly listed a `status` column; fixed 2026-08-02. Bench state lived in the KV `BENCH` namespace until `0011` — moved to these D1 columns because KV's eventual consistency made bench state flap across requests, its 1-write/sec-per-key limit made the bench key a hotspot under bursts, and the account row is already read on every dispatch, so the columns ride along with zero extra reads.)

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
| count_tokens_url | TEXT | nullable; **openai format only** (`0012_custom_provider_count_tokens_url.sql`). A **complete** URL for an Anthropic-shaped `/v1/messages/count_tokens` endpoint — nothing is appended to it. Same validator as `base_url`. `NULL` = the surface stays unsupported and `POST /anthropic/v1/messages/count_tokens` keeps returning its `400` ([providers.md](./providers.md) § Custom endpoints) |
| models_mode | TEXT | `auto` \| `manual`; `NOT NULL DEFAULT 'auto'` |
| manual_models_json | TEXT | nullable; JSON array of upstream model id strings |
| sort_order | INTEGER | display order within the user's list, ascending; `NOT NULL DEFAULT 0` (`0007_custom_provider_sort_order.sql`) |
| created_at | TEXT | |
| updated_at | TEXT | |

`sort_order` is **display only** — it never affects routing, pooling, or failover (a custom provider is selected by slug, not by list position; within-provider key priority stays in `upstream_accounts.priority`). Backfilled by `created_at ASC` so existing lists keep their current visual order. Reads sort `ORDER BY sort_order ASC, created_at ASC`, so ties and a all-zero legacy table degrade to the old behavior. Writes renumber the user's full list to a dense `0..n-1` sequence in one transaction rather than patching single rows; a create appends at the end.

`UNIQUE(user_id, slug)`. No `status` column here either — same computed-from-KV-bench convention as `upstream_accounts`, over that provider's account row(s).

**Slug reservation vs. existing rows.** When a slug later becomes a builtin provider id, reserving it only blocks *new* creates — an already-stored custom provider under that slug would be shadowed by the builtin lookup and become unreachable. `0013_rename_custom_antigravity_slug.sql` is the pattern: a data migration renames the stored slug (`antigravity` → `antigravity-custom`) and rewrites everything keyed by it in the same step (`upstream_accounts.provider`, `provider_settings.provider`, `model_groups.targets_json` prefixes). A user who already owns the target slug falls through to `antigravity-custom-2`, then `-3` — static passes because the targets_json rewrite is a string replace that must know the exact replacement. Each pass first deletes an *orphaned* `provider_settings` row under its target name (deleting a custom provider leaves that row behind as inert data, and it would collide with the `(user_id, provider)` primary key on rename). `request_logs.provider` is left as history.

### `model_groups`

User-defined **virtual endpoints** (full contract in [providers.md](./providers.md) § Model groups): each row is one group — name, URL slug, strategy — whose callable models live in `model_group_models`. Added in `0008_model_groups.sql`; rebuilt to the endpoint shape by `0014_group_endpoints.sql` (see below).

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | `ON DELETE CASCADE` |
| name | TEXT | **display name**: trimmed, 1–64 chars, free text, a label only. Unique per user. Mutable |
| slug | TEXT | the endpoint's URL id (`/g/<slug>/…`): custom-provider slug shape (`^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$`), unique per user, **mutable** (renaming moves the endpoint URL — nothing else references it). Added in `0014_group_endpoints.sql` |
| strategy | TEXT | `NOT NULL DEFAULT 'ordered'`; how the group orders its candidates ([providers.md](./providers.md) § Routing module). `ordered` is the only accepted value today — unknown values are rejected at write time, and reads treat an unrecognized stored value as `ordered` (forward compat). Added in `0010_routing_strategy.sql` |
| created_at | TEXT | |
| updated_at | TEXT | |

`UNIQUE(user_id, name)`, `UNIQUE(user_id, slug)`. No `status` column — usability is computed per-request from the targets' pools. The pre-v4 `targets_json` column is gone: targets now live per model row.

### `model_group_models`

The callable models of a group, 1–20 per group, each with its own ordered target list ([providers.md](./providers.md) § Model groups). Added in `0014_group_endpoints.sql`, replacing `model_group_aliases`.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | `ON DELETE CASCADE` |
| group_id | TEXT FK | → `model_groups.id`, `ON DELETE CASCADE` |
| name | TEXT | the callable model id on that group's endpoint: trimmed, 1–128 chars, no whitespace, `/` allowed |
| targets_json | TEXT | JSON array of target objects `{model: "provider/model", account_id?}`, array order = priority; `account_id` (nullable) pins the target to one `upstream_accounts` row — no FK, a deleted account makes the target skip at resolve time, mirroring the custom-provider convention. Bare strings are accepted as `{model}` shorthand. Parse must tolerate further per-target fields (future balancing weights) |
| created_at | TEXT | |
| updated_at | TEXT | |

`UNIQUE(group_id, name)` — a name resolves to exactly one model **within its group**; different groups may reuse a name (the endpoint is the namespace, so the v3 per-user uniqueness constraint is gone). Model resolution on a group endpoint reads this table via `(group_id, name)` (indexed lookup), never a JSON scan. Targets are validated at write time (prefix must be a builtin or the caller's own custom slug; never a bare name, so groups cannot nest).

**Migration `0014_group_endpoints.sql` (v3 → v4).** Rebuilds `model_groups` without `targets_json` and with `slug` (backfilled deterministically from the row id — `'g-' || substr(id, 6, 13)`, valid per the slug regex and collision-free since ids are unique; operators rename it to something meaningful in the UI). Creates `model_group_models` and seeds **one model per former alias**, each copying its group's whole `targets_json` — every `(group, alias)` pair that was callable before maps to a `(slug, model name)` pair that is callable on the group's endpoint, with identical routing. Then drops `model_group_aliases`. `request_logs.group_name` values written before v4 (bare aliases) are left as history.

### `provider_settings`

Per-user, per-provider-pool routing config ([providers.md](./providers.md) § Routing module) — the pool-level counterpart of `model_groups.strategy`, governing **direct** `provider/model` calls. Added in `0010_routing_strategy.sql`. `provider` is a builtin id **or** a custom provider's slug (same convention as `upstream_accounts.provider`); no FK to `custom_providers` — a row for a deleted slug is inert, mirroring how account rows are handled. **A missing row means `ordered`** — rows are created lazily on first write, never backfilled.

| Column | Type | Notes |
|--------|------|-------|
| user_id | TEXT | `ON DELETE CASCADE` → `users.id`; PK part |
| provider | TEXT | builtin id or custom slug; PK part |
| strategy | TEXT | `NOT NULL DEFAULT 'ordered'`; same value set and forward-compat rule as `model_groups.strategy` |
| updated_at | TEXT | ISO |

`PRIMARY KEY (user_id, provider)`.

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
| upstream_status | INTEGER | nullable; the last upstream HTTP status observed while serving this request (`0011_bench_and_refresh_state.sql`). `NULL` = no upstream response (pre-dispatch failure, transport failure before headers). Diagnoses what `status_code` hides: an eager stream logs `status_code: 200` and a synthesized `503` masks the bench-status that exhausted the pool — this column keeps the real upstream answer (see [logging.md](./logging.md)) |
| group_name | TEXT | nullable; `<group slug>/<model name>` when the request came through a group endpoint (`0008_model_groups.sql`; endpoint semantics since `0014` — rows written before v4 hold the bare alias, left as history). `provider`/`model` always store the **expanded** target so pricing and Overview aggregation stay canonical; this column preserves which endpoint model the request was addressed to |
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
| `BENCH` | `kano-proxy-bench` | **Deprecated since `0011`** — bench state moved to `upstream_accounts.bench_until`/`bench_reason` (see above). The binding stays declared so existing configs keep deploying, but no code path reads or writes it; remove the namespace in a later cleanup |
| `CACHE` | `kano-proxy-cache` | models list cache, codex/grok reasoning replay cache |

D1 database name: **`kano-proxy`** (Worker script name is also `kano-proxy`).

## Migrations

```bash
cd apps/api
npx wrangler d1 migrations create kano-proxy --local "init"
npx wrangler d1 migrations apply kano-proxy --local
npx wrangler d1 migrations apply kano-proxy --remote
```
