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

### `upstream_accounts`

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| user_id | TEXT FK | |
| provider | TEXT | builtin `claude-code` \| `codex` \| `grok`, **or** a custom provider's `slug` (see `custom_providers` below) |
| external_account_id | TEXT | nullable; upstream account id when known (e.g. codex's ChatGPT account id) |
| label | TEXT | email or display |
| priority | INTEGER | higher = preferred; promote bumps |
| encrypted_payload | TEXT | AES-GCM blob: tokens + provider fields. For a custom provider this is just `{access_token: <api key>}` |
| account_meta_json | TEXT | email, plan, non-secret. For a custom provider: `{key_mask: "sk-abc…f3a2"}` (see [providers.md](./providers.md)) |
| created_at | TEXT | |
| updated_at | TEXT | |

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
| created_at | TEXT | |
| updated_at | TEXT | |

`UNIQUE(user_id, slug)`. No `status` column here either — same computed-from-KV-bench convention as `upstream_accounts`, over that provider's account row(s).

### `usage_snapshots`

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | |
| account_id | TEXT FK | |
| fetched_at | TEXT | |
| payload_json | TEXT | windows, stale flag |
| stale | INTEGER | 0/1 |

Optional cache; may also use KV with 60s TTL.

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
| prompt_tokens | INTEGER | nullable |
| completion_tokens | INTEGER | nullable |
| error_code | TEXT | nullable |
| created_at | TEXT | |

**No message content, no prompts, no completions.**

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
