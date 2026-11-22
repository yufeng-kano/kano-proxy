# Database (PostgreSQL)

Migrations live in `apps/api/crates/kano-proxy-core/migrations/` and are applied by the server at start (§ Migrations). `0001_core_baseline.sql` creates every table below unless its section names a later migration; a column whose notes name a migration was added by it.

## Tables

### `users`

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | uuid |
| google_sub | TEXT UNIQUE | OIDC subject |
| email | TEXT | |
| name | TEXT | nullable |
| picture_url | TEXT | nullable |
| created_at | TEXT | ISO |
| updated_at | TEXT | ISO |
| notice_version | TEXT | nullable (`0006_usage_notice.sql`). The usage-notice version the user last accepted ([auth.md](./auth.md) § Usage notice); NULL until the first acceptance |
| notice_accepted_at | TEXT | nullable (`0006`). ISO time of that acceptance |

### `notice_acceptances`

Every usage-notice version a user accepted, kept so an earlier acceptance is not lost when the notice changes. Written only by `POST /api/auth/notice`, in the same transaction as the two `users` columns. Deleted with the user.

| Column | Type | Notes |
|--------|------|-------|
| user_id | TEXT FK | `users.id`, `ON DELETE CASCADE` |
| version | TEXT | the accepted version |
| accepted_at | TEXT | ISO |

Primary key `(user_id, version)`. Added by `0006_usage_notice.sql`.

### `sessions`

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | random |
| user_id | TEXT FK | `ON DELETE CASCADE` |
| expires_at | TEXT | 14 days after creation; the cookie's signature proves the id, this row its expiry ([auth.md](./auth.md)) |
| created_at | TEXT | |

### `api_keys`

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | `ON DELETE CASCADE` |
| name | TEXT | user label |
| key_prefix | TEXT | the key's first 20 characters, for display |
| key_hash | TEXT UNIQUE | hex SHA-256 of the full key — the plaintext is never stored ([auth.md](./auth.md) § Client API keys) |
| created_at | TEXT | |
| last_used_at | TEXT | nullable |
| spend_limit | DOUBLE PRECISION | nullable; USD ceiling per window, `NULL` = unlimited ([pricing.md](./pricing.md)) |
| spend_limit_interval | TEXT | `daily` \| `weekly` \| `monthly` \| `total`; `NOT NULL DEFAULT 'monthly'` |
| spend_limit_include_oauth | INTEGER | 0/1, `NOT NULL DEFAULT 1`; whether builtin-provider (subscription OAuth) traffic counts toward the limit |
| device_id | TEXT FK | nullable; the `cli_devices` row that minted the key through `POST /cli/v1/keys` ([cli.md](./cli.md) § Keys and groups), `ON DELETE CASCADE` — revoking the device deletes its keys. `NULL` for keys created in the web UI. Indexed |

`device_id` is added by `0003_api_keys_device.sql`.

### `upstream_accounts`

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | `ON DELETE CASCADE` |
| provider | TEXT | builtin `claude-code` \| `codex` \| `grok` \| `antigravity`, **or** a custom provider's `slug` (see `custom_providers` below), **or** a CLI provider's `slug` (its one internal state row — see `cli_providers` below) |
| external_account_id | TEXT | nullable; upstream account id when known (e.g. codex's ChatGPT account id) |
| label | TEXT | nullable; email or display name, **synced from upstream** on every usage refresh (an accounts read past the usage TTL, or the background refresh) — not user-editable. A CLI provider's internal row carries the provider's display name instead, rewritten by its rename |
| custom_label | TEXT | nullable; the operator's own name for this account. Wins over `label` for display and is **never** overwritten by the upstream sync |
| priority | INTEGER | `NOT NULL DEFAULT 0`; higher = preferred. A new bind lands on top (the pool's maximum + 1); a reorder renumbers the pool `n..1` |
| encrypted_payload | TEXT | AES-GCM blob: tokens + provider fields. For a custom provider this is just `{access_token: <api key>}` |
| account_meta_json | TEXT | email, plan, non-secret. For a custom provider: `{key_mask: "sk-abc…f3a2"}` (see [providers.md](./providers.md)). For codex also `credits: {has_credits, unlimited, balance} \| null` from the last successful usage read ([providers.md](./providers.md) § Codex "Credit spillover") |
| usage_snapshot_json | TEXT | nullable; last usage read — `{windows, account?, error, stale, edgeBlocked}`; a failed fetch keeps the prior windows and marks them `stale` |
| usage_fetched_at | TEXT | nullable; when `usage_snapshot_json` was written. Drives the 2 min server-side TTL |
| usage_fetching_at | TEXT | nullable; lock holder's timestamp while an upstream fetch is in flight (`NULL` = free; breakable after 30s) |
| bench_until | TEXT | nullable; ISO time until which this account is benched. `NULL` or past = not benched — expired values are compared at read, never proactively deleted. Writes are **monotonic**: a bench write only lands when it extends (`bench_until IS NULL OR bench_until < new`), so a shorter concurrent penalty can never truncate a longer one ([providers.md](./providers.md) § Routing module) |
| bench_reason | TEXT | nullable; content-free cause of the current bench — the upstream status as text (`"401"`, `"429"`, `"524"`, …) or, on a CLI provider's internal row, the tunnel fault `"offline"`. Written with `bench_until`, nulled by unpause, and with it by a successful OAuth refresh when the reason is `"401"` ([providers.md](./providers.md) § Sign-in state) |
| bench_scopes_json | TEXT | nullable; benches that cover one **quota scope** of the account instead of all of it (`0004_scoped_bench.sql`, [providers.md](./providers.md) § Routing module "Quota scopes"): `{"<scope>": {"until": "<ISO>", "reason": "429"}}`. Same rules as `bench_until` per entry — monotonic per scope, compared at read, expired entries never deleted (at most one per scope the adapter defines). Nulled by unpause |
| allow_credits | BOOLEAN | `NOT NULL DEFAULT FALSE`; the owner's opt-in to **credit spillover** (`0005_account_allow_credits.sql`, [providers.md](./providers.md) § Codex "Credit spillover"): when true and the stored `credits` read usable, an exhausted usage window no longer makes the account unusable. Written only by `PATCH /api/providers/:provider/accounts/:id`, and only for a provider whose adapter supports spillover (`codex` today). Reconnect, unpause and the usage refresh leave it alone |
| strip_fast_mode | BOOLEAN | `NOT NULL DEFAULT FALSE`; **Remove fast mode** for this account (`0008_account_request_edits.sql`, [providers.md](./providers.md) § Request edits). Written only by `PATCH /api/providers/:provider/accounts/:id`, which refuses it for `grok` / `antigravity` (neither forwards a fast-mode field). Reconnect, unpause and the usage refresh leave it alone |
| strip_claude_code_line | BOOLEAN | `NOT NULL DEFAULT FALSE`; **Hide Claude Code identity** for this account (`0008`). Same writer, accepted for every provider, same rule for reconnect, unpause and the usage refresh |
| force_cache_ttl_1h | BOOLEAN | `NOT NULL DEFAULT FALSE`; **Keep prompt cache for 1 hour** for this account (`0010_account_cache_ttl.sql`, [providers.md](./providers.md) § Request edits). Same writer, accepted only where the upstream reads Anthropic `cache_control` (`claude-code`, Anthropic-format custom endpoints and CLI providers), same rule for reconnect, unpause and the usage refresh. Added off on every existing row, so an upgrade changes no request |
| refreshing_at | TEXT | nullable; OAuth refresh single-flight lock, same CAS pattern as `usage_fetching_at` (30s breakable) — see [providers.md](./providers.md) § OAuth refresh single-flight |
| refresh_failure_json | TEXT | nullable; the last failed OAuth refresh, `{at, status, refused, expires_at}` (`0009_sign_in_state.sql`, [providers.md](./providers.md) § Sign-in state). `status` is the token endpoint's HTTP status, `null` when it did not answer usably; `refused` is true only when the answer was `invalid_grant`; `expires_at` is the stored access token's expiry when the refresh failed. Written by the refresh single-flight in the statement that releases `refreshing_at`; nulled by a successful refresh, by reconnect, and by a `2xx` through the account when the record says the token had run out. Never written by unpause |
| sign_in_rejected_at | TEXT | nullable; when upstream last answered `401` for a request through this account (`0009`, builtin providers only). Written by dispatch beside the bench; nulled by a successful refresh, by reconnect, and by any `2xx` through the account. It is what keeps an expired sign-in reading `reauth` after its bench runs out |
| edge_strikes | INTEGER | `NOT NULL DEFAULT 0`; consecutive upstream edge-timeout (520/522/524) strikes. Incremented atomically on each edge-timeout; the 3rd within 10 minutes benches 30s and resets to 0 ([providers.md](./providers.md) § Routing module "Penalties"). Never written on success |
| edge_strike_at | TEXT | nullable; time of the last edge-timeout strike. A value older than 10 minutes stales the count — the next increment restarts at 1 |
| created_at | TEXT | |
| updated_at | TEXT | |

`allow_credits`, the request-edit switches and `force_cache_ttl_1h` are `BOOLEAN` rather than the baseline's `INTEGER` 0/1 flags: they read straight into a Rust `bool`.

`0008` moved the request-edit switches from the provider to the account, copying each user's `provider_settings` pair for a provider onto every account that user owns under it (builtin, custom or CLI). A shared account carries its owner's switches, which are the only ones that apply when it is lent ([providers.md](./providers.md) § Request edits "Shared accounts").

`label` and `custom_label` are two different jobs and must not be merged: `label` is a cache of upstream identity (the accounts read overwrites it whenever the upstream email/display name changes), so a rename written there survives only until the next poll. `custom_label` is user intent — set by `PATCH /api/providers/:provider/accounts/:id`, cleared by sending `null`/`""`, and read first by the display-name resolver.

**There is no persisted `status` column.** "Active / standby / limited / benched / reauth" (or, for a custom provider, the simpler "active" / "benched") is computed at read time from `priority` order, the bench columns above (`pool/bench.rs`), the sign-in columns (`pool/sign_in.rs`) and the stored `usage_snapshot_json` windows (`routing/facts.rs`), never stored. Bench state lives on the row, not in a cache: a write is visible to the very next request, and the account row is already read on every dispatch, so the columns ride along with zero extra reads.

Indexed on `(user_id, provider)`. There is no uniqueness constraint on `external_account_id`: binding the same upstream account twice creates two rows; reconnect (`POST /api/providers/:provider/login` with `{account_id}`, [auth.md](./auth.md)) is how an existing row takes a new credential.

### `custom_providers`

User-defined custom upstream providers (BYO endpoint + API key — see [providers.md](./providers.md)). Its API key(s) are ordinary `upstream_accounts` rows with `provider = slug`; this table only holds the provider-level config, and has no FK from `upstream_accounts` back to it (deleting a custom provider deletes its account rows at the application layer, not via `ON DELETE CASCADE`).

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | `ON DELETE CASCADE` |
| slug | TEXT | unique per user; renameable through `PUT /api/custom-providers/:id`, which rewrites `upstream_accounts.provider`, `provider_settings.provider` and the user's group targets in the same transaction ([providers.md](./providers.md) § Custom endpoints) |
| name | TEXT | display name |
| format | TEXT | `openai` \| `anthropic`; immutable after creation |
| base_url | TEXT | validated (https, no credentials/query/fragment, not localhost/private/loopback/own-host) and trailing-slash-stripped on save |
| count_tokens_url | TEXT | nullable; **openai format only**. A **complete** URL for an Anthropic-shaped `/v1/messages/count_tokens` endpoint — nothing is appended to it. Same validator as `base_url`. `NULL` = the surface stays unsupported and `POST /anthropic/v1/messages/count_tokens` keeps returning its `400` ([providers.md](./providers.md) § Custom endpoints) |
| models_mode | TEXT | `auto` \| `manual`; `NOT NULL DEFAULT 'auto'` |
| manual_models_json | TEXT | nullable; JSON array of upstream model id strings |
| sort_order | INTEGER | display order within the user's list, ascending; `NOT NULL DEFAULT 0` |
| created_at | TEXT | |
| updated_at | TEXT | |

`sort_order` is **display only** — it never affects routing, pooling, or failover (a custom provider is selected by slug, not by list position; within-provider key priority stays in `upstream_accounts.priority`). Reads sort `ORDER BY sort_order ASC, created_at ASC`, so ties fall back to creation order. Writes renumber the user's full list to a dense `0..n-1` sequence in one transaction rather than patching single rows; a create appends at the end.

`UNIQUE(user_id, slug)`. No `status` column here either — same computed-from-bench convention as `upstream_accounts`, over that provider's account row(s).

**Slug reservation vs. existing rows.** When a slug later becomes a builtin provider id, reserving it only blocks *new* creates — an already-stored custom provider under that slug would be shadowed by the builtin lookup and become unreachable. Reserving one therefore takes a data migration that renames the stored slug (as `antigravity` became `antigravity-custom` when Antigravity became builtin) and rewrites everything keyed by it in the same step: `upstream_accounts.provider`, `provider_settings.provider` and the group target prefixes in `model_group_models.targets_json`. A user who already owns the new slug falls through to `-2`, then `-3`, as static passes, because the targets rewrite is a string replace that must know the exact replacement. Each pass first deletes a `provider_settings` row already under its target name (it would collide with the `(user_id, provider)` primary key). `request_logs.provider` is left as history. The runtime slug rename (`db::custom_providers::rename_custom_provider_slug`) follows the same rewrite set, per user and in one transaction.

### `cli_devices`

One machine that ran `kano-proxy login` (contract: [cli.md](./cli.md)).

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | `ON DELETE CASCADE` |
| name | TEXT | 1–64 chars, trimmed |
| refresh_token_hash | TEXT | SHA-256 of the current rotating refresh token — never the plaintext |
| refresh_token_prev_hash | TEXT | nullable; hash of the immediately superseded token. A refresh presenting a token that matches **this** column is reuse-as-theft and revokes the device ([cli.md](./cli.md) § Device auth); a token matching neither column is a plain 401 |
| last_seen_at | TEXT | nullable; bumped on token rotation and tunnel connect |
| created_at | TEXT | |

Deleting a device cascades to `api_keys.device_id` (the keys it minted) and nulls nothing else — `cli_providers.device_id` is informational and keeps its value until the provider is removed.

### `cli_login_requests`

Pending `kano-proxy login` authorize-then-paste logins ([cli.md](./cli.md) § Device auth). Expired rows are purged by the daily retention sweep ([logging.md](./logging.md)).

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | the `request_id` the CLI holds |
| device_name | TEXT | requested at start, becomes the device row's name on complete |
| ip_hash | TEXT | nullable; SHA-256 of the requesting IP — the per-IP start budget is an atomic conditional INSERT counting recent rows with this hash ([cli.md](./cli.md) § Security notes). The raw address is never stored |
| code_hash | TEXT | nullable; SHA-256 of the normalized one-time code — written on approve, never before |
| user_id | TEXT | NULL until approved; the approving session's user |
| expires_at | TEXT | start + 10 minutes |
| approved_at | TEXT | nullable |
| used_at | TEXT | nullable; a redeemed request can never be redeemed again |
| attempts | INTEGER | `NOT NULL DEFAULT 0`; wrong-code attempts — the request is dead after 5 |
| created_at | TEXT | |

### `cli_providers`

One local endpoint registered by `kano-proxy tunnel add` (contract: [cli.md](./cli.md)). Each row also owns one `upstream_accounts` row (`provider = slug`, placeholder credential) — internal routing state only, created and deleted with the provider, never shown as an account. The local target URL and its optional API key are **not stored here** — they are CLI state on the device.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | also the tunnel registry key |
| user_id | TEXT FK | `ON DELETE CASCADE` |
| device_id | TEXT | nullable, informational "registered from" — no FK enforcement; a deleted device leaves the provider intact |
| slug | TEXT | custom-provider slug rules, immutable, unique per user **across both** `custom_providers` and `cli_providers` (both create paths check the other table) |
| name | TEXT | display name, 1–64 chars; renameable from the Providers page's CLI section |
| format | TEXT | `openai` \| `anthropic`; immutable (delete-and-recreate) |
| models_json | TEXT | nullable; the last agent-reported model list, stored whole (JSON array of id strings) |
| models_updated_at | TEXT | nullable; when the report was written |
| model_filter_json | TEXT | nullable; expose-whitelist applied at read time over the stored report — clearing it never loses data |
| sort_order | INTEGER | `NOT NULL DEFAULT 0`; display order, same convention as `custom_providers` |
| created_at | TEXT | |
| updated_at | TEXT | |

`UNIQUE(user_id, slug)` — and the cross-table half of the shared namespace is enforced inside the INSERT itself (`… SELECT … WHERE NOT EXISTS (custom_providers row)`, mirrored on the custom create), so concurrent creates cannot race past a check-then-insert. CLI providers count into the same 20-per-user cap as custom providers (shared across both tables); ≤ 20 devices per user. Revoking deletes the device row (`0002_cli_devices_delete_on_revoke.sql`), so refresh and connect find nothing and live sockets die at access-token expiry.

### `model_groups`

User-defined **virtual endpoints** (full contract in [providers.md](./providers.md) § Model groups): each row is one group — name, URL slug, strategy — whose callable models live in `model_group_models`.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | `ON DELETE CASCADE` |
| name | TEXT | **display name**: trimmed, 1–64 chars, free text, a label only. Unique per user. Mutable |
| slug | TEXT | the endpoint's URL id (`/g/<slug>/…`): custom-provider slug shape (`^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$`), unique per user, **mutable** (renaming moves the endpoint URL — nothing else references it) |
| strategy | TEXT | `NOT NULL DEFAULT 'ordered'`; how the group orders its candidates ([providers.md](./providers.md) § Routing module). `ordered` is the only accepted value today — unknown values are rejected at write time. Reads return the stored value; dispatch does not read it, because `ordered` is the only value that can be written |
| created_at | TEXT | |
| updated_at | TEXT | |

`UNIQUE(user_id, name)`, `UNIQUE(user_id, slug)`. No `status` column — usability is computed per-request from the targets' pools. Targets live per model row (`model_group_models`).

### `model_group_models`

The callable models of a group, 1–20 per group, each with its own ordered target list ([providers.md](./providers.md) § Model groups).

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | `ON DELETE CASCADE` |
| group_id | TEXT FK | → `model_groups.id`, `ON DELETE CASCADE` |
| name | TEXT | the callable model id on that group's endpoint: trimmed, 1–128 chars, no whitespace, `/` allowed |
| targets_json | TEXT | JSON array of target objects `{model: "provider/model", account_id?}`, array order = priority; `account_id` (nullable) pins the target to one `upstream_accounts` row — no FK, a deleted account makes the target skip at resolve time, mirroring the custom-provider convention. Stored targets are always objects: the write API normalizes the bare-string `"provider/model"` shorthand ([auth.md](./auth.md) § Model groups) to `{model}` before storing. The parse ignores further per-target fields and drops an entry without a string `model` |
| created_at | TEXT | |
| updated_at | TEXT | |

`UNIQUE(group_id, name)` — a name resolves to exactly one model **within its group**; different groups may reuse a name (the endpoint is the namespace). Model resolution on a group endpoint reads this table via `(group_id, name)` (indexed lookup), never a JSON scan. Targets are validated at write time (prefix must be a builtin or the caller's own custom or CLI slug; never a bare name, so groups cannot nest).

### `provider_settings`

Per-user, per-provider routing **strategy** ([providers.md](./providers.md) § Routing module), the pool-level counterpart of `model_groups.strategy`, governing **direct** `provider/model` calls.

`provider` is a builtin id, a custom provider's slug **or** a CLI provider's slug (same convention as `upstream_accounts.provider`); no FK to `custom_providers` / `cli_providers`. Only builtin ids are written today. Deleting a custom endpoint or CLI provider deletes its row in the same transaction, and creating one deletes any row already under its slug in the insert's transaction, so a new provider never inherits an old one's row. **A missing row means `ordered`** — rows are created lazily on first write, never backfilled.

`strip_fast_mode` and `strip_claude_code_line` (`0007_provider_request_edits.sql`) held the request-edit switches per provider until `0008` copied them onto each account (`upstream_accounts` above). Since `0008` nothing reads or writes them. Dropping them takes a new migration, and rules out running a core build from before `0008` (which reads them on every request) against the database.

| Column | Type | Notes |
|--------|------|-------|
| user_id | TEXT | `ON DELETE CASCADE` → `users.id`; PK part |
| provider | TEXT | builtin id, custom slug or CLI slug; PK part |
| strategy | TEXT | `NOT NULL DEFAULT 'ordered'`; same value set and read rule as `model_groups.strategy` |
| strip_fast_mode | BOOLEAN | `NOT NULL DEFAULT FALSE`; unread since `0008` (above) |
| strip_claude_code_line | BOOLEAN | `NOT NULL DEFAULT FALSE`; unread since `0008` (above) |
| updated_at | TEXT | ISO |

`PRIMARY KEY (user_id, provider)`.

### `usage_snapshots` — **never used**

In the baseline, never read or written by any code. The usage cache lives in the `upstream_accounts` columns above; a separate table would cost one extra write per refresh and a second query per read, for nothing. Left in place rather than dropped — an empty table is free, and a `DROP TABLE` migration is a schema change with no benefit. **Do not build on it.**

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
| user_id | TEXT | no FK |
| api_key_id | TEXT | nullable; no FK — a deleted key leaves its rows |
| provider | TEXT | |
| model | TEXT | |
| account_id | TEXT | nullable |
| status_code | INTEGER | |
| latency_ms | BIGINT | |
| prompt_tokens | BIGINT | nullable; **total** input tokens, including cached reads and cache writes |
| completion_tokens | BIGINT | nullable |
| cache_read_input_tokens | BIGINT | nullable; input tokens served from upstream prompt cache |
| cache_creation_input_tokens | BIGINT | nullable; cache-write tokens, from Anthropic `cache_creation_input_tokens`, OpenAI-compatible `prompt_tokens_details.cache_write_tokens`, or the proxy's conversion extension |
| cost | DOUBLE PRECISION | nullable; estimated USD cost computed at write time from the LiteLLM price table ([pricing.md](./pricing.md)); `NULL` = unpriced/unknown, never 0-as-guess |
| error_code | TEXT | nullable |
| upstream_status | INTEGER | nullable; the last upstream HTTP status observed while serving this request. `NULL` = no upstream response (pre-dispatch failure, transport failure before headers). Diagnoses what `status_code` hides: an eager stream logs `status_code: 200` and a synthesized `503` masks the bench-status that exhausted the pool — this column keeps the real upstream answer (see [logging.md](./logging.md)) |
| api_key_name | TEXT | nullable; the key's `api_keys.name` **as it was when the row was written**. The Logs page prefers the live name and falls back to this when the key was deleted since, so a removed key still reads by its last name. `NULL` on rows written before the column existed or when the key was already gone at write time |
| account_label | TEXT | nullable; the serving account's display label (`custom_label` \|\| `label`) as it was at write time. Same read rule: live label first, this as the fallback for a removed account |
| group_name | TEXT | nullable; `<group slug>/<model name>` when the request came through a group endpoint (rows written before group endpoints existed hold a bare alias, left as history). `provider`/`model` always store the **expanded** target so pricing and Overview aggregation stay canonical; this column preserves which endpoint model the request was addressed to |
| created_at | TEXT | |
| started_at | TEXT | nullable; when dispatch started (UTC ISO), captured before the candidate walk and carried to the row on every dispatch path (eager stream, non-stream and audio delivery), independent of `created_at` (write time) and `latency_ms` (TTFB). A row written outside dispatch gets its write time. Always written now, but `NULL` on older rows, so a consumer needing a start boundary falls back to `created_at` |

**No message content, no prompts, no completions.**

Token semantics (normalized across providers — capture matrix in [logging.md](./logging.md)):

- `prompt_tokens` is always the **total** input count. Anthropic-shaped usage reports `input_tokens` *excluding* cache reads/writes, so the logged value is `input_tokens + cache_read_input_tokens + cache_creation_input_tokens`; OpenAI-shaped `prompt_tokens` already includes cached tokens and is stored as-is.
- `cache_read_input_tokens` maps from Anthropic `cache_read_input_tokens`, OpenAI `prompt_tokens_details.cached_tokens`, or Responses `input_tokens_details.cached_tokens`. `cache_creation_input_tokens` maps from Anthropic `cache_creation_input_tokens`, OpenAI-compatible `prompt_tokens_details.cache_write_tokens`, or the proxy's `cache_creation_input_tokens` conversion extension.
- `NULL` means *unreported*, not zero: when an Anthropic-shaped `usage` is present, absent cache fields default to `0` (the API defines them); OpenAI-shaped usage stores `NULL` unless the corresponding `prompt_tokens_details` member (or the `cache_creation_input_tokens` extension) was actually present. Cache-rate aggregation divides only over rows where `cache_read_input_tokens IS NOT NULL`.
- Streamed requests write their row when the stream ends, so token fields can be populated; a client that disconnects mid-stream still gets a row with whatever usage was seen by then.

The writer reads the name snapshots (`api_key_name`, `account_label`) by primary key just before the insert, off the request path, so a key or account deleted in that window simply leaves `NULL`. Indexes: `request_logs_api_key_created_idx` on `(api_key_id, created_at)` serves the spend-limit sum; `request_logs_user_created_idx` on `(user_id, created_at)` serves dashboard range queries.

Rows past the retention window (default 90 days) are deleted by the daily sweep, which also purges expired `sessions`, `oauth_login_states` and `cli_login_requests` rows — see [logging.md](./logging.md) § Retention sweep.

### `oauth_login_states`

Pending provider or Google login state (PKCE verifier, expiry; `reconnect_account_id` inside `payload_json` when the login reconnects an existing account — [auth.md](./auth.md) § Management routes). Short-lived: a completed login deletes its row, expired rows are pruned on the provider-login path that adds rows and by the daily retention sweep.

## Caches

Nothing cached is persisted. The model catalog, the Codex and Grok reasoning replay caches, the price table ([pricing.md](./pricing.md)) and the changelog live in bounded in-process caches with their own TTLs; a restart simply refetches.

Bench state is not a cache: it is `upstream_accounts.bench_until` / `bench_reason`, on the row itself.

## Migrations

Migrations are plain SQL files in `apps/api/crates/kano-proxy-core/migrations/`, embedded in the binary through the `db::CORE_MIGRATIONS` list (in filename order) and applied in that order when the server starts, each inside one transaction, and recorded in `core_migrations`. Restarting the same build applies nothing.

Applied migrations are immutable. A schema change is a new file plus its entry at the end of `db::CORE_MIGRATIONS`; never an edit to one already released. An edition composing this core keeps its own history table and applies after the core.
