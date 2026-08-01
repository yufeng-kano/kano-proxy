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
| provider | TEXT | `claude-code` \| `codex` \| `grok` |
| label | TEXT | email or display |
| priority | INTEGER | higher = preferred; promote bumps |
| status | TEXT | active metadata; runtime bench in KV |
| encrypted_payload | TEXT | AES-GCM blob: tokens + provider fields |
| account_meta_json | TEXT | email, plan, non-secret |
| created_at | TEXT | |
| updated_at | TEXT | |

Unique optional: `(user_id, provider, external_account_id)` when known.

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
