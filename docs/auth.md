# Auth

## Admin UI — Google OIDC

- **No password login.**
- Flow: Authorization Code + PKCE (preferred) or code flow suitable for Workers.
- Session: HTTP-only cookie (`kano-proxy_session`), HMAC-signed with `SESSION_SECRET`. The `Secure` attribute is set whenever the request that issued it was HTTPS — `createSession(env, userId, { secure })` / `clearSessionCookie(secure)` take an explicit flag; the callback and logout routes derive it from that request's own `protocol` — so local HTTP dev still gets a working cookie while production (always HTTPS behind the Worker) gets `Secure`.
- Cookie signature verification (`loadSessionUser`) uses a constant-time comparison (`timingSafeEqual` in `auth/session.ts`), not `!==`, so response timing cannot be used to guess the HMAC byte-by-byte.
- CSRF: state param on OAuth; same-site cookies for mutating `/api/*`.

### Config placeholders (never commit real values)

```text
GOOGLE_CLIENT_ID=
GOOGLE_CLIENT_SECRET=
GOOGLE_REDIRECT_URI=https://<your-domain>/api/auth/callback
SESSION_SECRET=
```

Local:

```text
GOOGLE_REDIRECT_URI=http://127.0.0.1:8787/api/auth/callback
```

### Routes

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/auth/login` | Redirect to Google |
| GET | `/api/auth/callback` | Exchange code, create session, 302 to `APP_URL/` |
| POST | `/api/auth/logout` | Clear session |
| GET | `/api/auth/me` | Current user profile |

Anyone with a Google account may register on first login (insert user row).

The callback redirects to the SPA root (`APP_URL/`), not to a specific page: landing on bare `/` is what lets the app restore the route the user was last on instead of dropping everyone on one fixed page (see [admin-ui.md](./admin-ui.md) § View preferences). Never leave the user on the Worker root — that is an API, not a UI.

## CORS

- `/api/*` (session-cookie-authenticated admin JSON): origin-locked to `APP_URL`'s origin, `credentials: true` — only the admin SPA can make cookie-authenticated cross-origin requests. A request whose `Origin` does not match `APP_URL` gets no `Access-Control-Allow-Origin` header at all (browser blocks it), never a wildcard.
- `/openai/*`, `/anthropic/*`, `/health`: permissive CORS (any origin), **no credentials**. Browser-based clients that authenticate with a project API key (never the session cookie) must be able to call these from any origin.
- The session-loading middleware stays global (`app.use("*", ...)`) regardless of path — only the CORS policy is split.

## Client API keys

- Created in admin UI; **plaintext shown once**.
- Stored as **hash only** (e.g. SHA-256 of full key) + prefix for display (`sk-kano-proxy-xxxx…`).
- No TTL; delete to revoke.
- Multiple keys per user.
- LLM requests resolve `key → user_id` then use **that user’s** pools only.
- Optional per-key **spend limit** (`spend_limit` USD + `spend_limit_interval` `daily|weekly|monthly|total` + `spend_limit_include_oauth`): enforced in the API-key auth middleware; at/over the window's estimated spend the LLM surfaces return **429** `spend_limit_exceeded` (Anthropic surface: `rate_limit_error`) before any upstream call. See [pricing.md](./pricing.md).

### Management routes (session required)

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/keys` | Each key carries its limit fields plus `window_spend` (estimated USD already spent in the current window) |
| POST | `/api/keys` | Body `{name?, spend_limit?, spend_limit_interval?, spend_limit_include_oauth?}`; plaintext key in the response, once |
| PATCH | `/api/keys/:id` | Update `{name?, spend_limit?, spend_limit_interval?, spend_limit_include_oauth?}`; `spend_limit: null` clears the limit |
| DELETE | `/api/keys/:id` | Revoke |
| GET | `/api/models` | Catalog with `available`; `?available=1` filters |
| GET | `/api/usage/summary` | Aggregated `request_logs` for the dashboard; `?days=` 1, 7, or 30 (default 7). `models[]` is per **(provider, model)** — no account/alias dimension (see [admin-ui.md](./admin-ui.md) § Series shape) |
| GET | `/api/logs` | Newest-first `request_logs` rows for the caller; `?limit=` (default 50, max 100), `?cursor=` opaque keyset over `(created_at, id)`, `?provider=` exact slug, `?errors=1` (`error_code` set or `status_code >= 400`). Returns `{rows, next_cursor}`; rows carry read-time `account_label` / `api_key_name`, derived `usage_type` (`oauth` = builtin provider, `api` = anything else), and read-time cost fill for `NULL` `cost` |

## Upstream OAuth (account binding)

Secrets for public OAuth client ids may use well-known CLI defaults (override via env). Refresh tokens encrypted at rest with `TOKEN_ENCRYPTION_KEY`.

### Claude Code

- **PKCE S256 required** (`code_challenge` / `code_verifier`).
- Browser authorize → user pastes `code#state` from Anthropic callback page.
- Token exchange must send `code_verifier`.
- Store access + refresh; multi-account pool.

### Codex

**Device code flow only.** The browser-redirect flow is gone: the public OpenAI client's only registered redirect is `http://localhost:1455/auth/callback` — unchangeable, and nothing listens there when the proxy runs the flow, so it left the user copying a dead URL out of the address bar. Device code needs no redirect the proxy can serve, so the dialog matches Grok's: show a code, wait.

- Endpoints (OpenAI-specific, **not** RFC 8628 `/oauth/device/code` — that path sits behind a Cloudflare challenge):
  - start: `POST https://auth.openai.com/api/accounts/deviceauth/usercode`
  - poll: `POST https://auth.openai.com/api/accounts/deviceauth/token`
  - exchange: `POST https://auth.openai.com/oauth/token`
- **The two `deviceauth` endpoints take JSON, not form encoding.** A `application/x-www-form-urlencoded` body is rejected `400 model_attributes_type` ("Input should be a valid dictionary or object") — the failure mode that made every sign-in attempt surface as "Couldn't start sign-in" before v2.8.1. Only the final `/oauth/token` exchange is form-encoded, per ordinary OAuth.
- Start request body is `{client_id}`. The response carries **`device_auth_id`** (not `device_code`), `user_code`, `interval` (a *string* in practice — coerce), and `expires_at`. It carries **no verification URI**: the proxy supplies `https://auth.openai.com/codex/device`, the page the CLI prints, and the user types the code there. Do not synthesize a prefilled `verification_uri_complete` — no such parameter is documented and a wrong one lands the user on a confusing page.
- Poll request body is `{device_auth_id, user_code}` — **both fields**; omitting `user_code` is a hard `400 Field required`. While the user has not approved, the endpoint answers `403` with `code: "deviceauth_authorization_pending"` inside a nested `error` object (so the RFC 8628 top-level `error: "authorization_pending"` string is absent — treat `403`/`404` as pending). On approval it returns `authorization_code` **and `code_verifier`**.
- **PKCE is server-side in this flow.** OpenAI generates the challenge/verifier pair and hands the verifier back in the poll response; the proxy generates nothing and sends no `code_challenge` at start. The exchange sends that server-issued `code_verifier` with `redirect_uri=https://auth.openai.com/deviceauth/callback` (a fixed protocol constant here, not a page anyone visits). A proxy-generated verifier cannot match the server's challenge and fails the exchange.
- Same `client_id` as the CLI (`CODEX_OAUTH_CLIENT_ID` overrides); the refresh path in `providers/codex.ts` is unchanged, since the tokens are ordinary OAuth tokens.
- These endpoints are undocumented by OpenAI and may change without notice. Device sign-in must also be permitted in the user's ChatGPT security settings — a persistent poll rejection there is a user-side setting, not a proxy bug.

### Grok

- Device-code flow in admin UI (poll until complete).
- Multi-account store (extend beyond single-token local tools).

### Management routes (session required)

| Method | Path |
|--------|------|
| GET | `/api/providers/:provider/accounts` |
| PATCH | `/api/providers/:provider` |
| POST | `/api/providers/:provider/login` |
| POST | `/api/providers/:provider/login/:id/complete` |
| GET | `/api/providers/:provider/login/:id` |
| POST | `/api/providers/:provider/accounts/:id/promote` |
| POST | `/api/providers/:provider/accounts/:id/unpause` |
| PATCH | `/api/providers/:provider/accounts/:id` |
| DELETE | `/api/providers/:provider/accounts/:id` |
| POST | `/api/providers/:provider/accounts/import` |
| GET | `/api/providers/:provider/usage?refresh=` |

`:provider` ∈ `claude-code` | `codex` | `grok`. `accounts/import` is a manual credential-ingest route (bootstrapping / tests) — same shape as a completed OAuth login, but the caller supplies `access_token` (and optional `refresh_token` / `expires_at` / `account_id` / `email` / `label`) directly instead of running the OAuth dance.

`PATCH /api/providers/:provider/accounts/:id` renames an account: body `{custom_label: string | null}`, trimmed, max 64 chars, `null`/`""` clears it and falls back to the upstream identity. It touches **only** `custom_label` — never tokens, priority, or the upstream-synced `label` (see [database.md](./database.md)) — and returns `{ok: true, custom_label}`. 404 when the id is not the caller's.

`POST /api/providers/:provider/accounts/:id/unpause` nulls the D1 `bench_until`/`bench_reason` columns for that account so it is eligible for acquire again ([providers.md](./providers.md) § Routing module "Manual unpause"). Session required. Returns `{ok: true}`. Idempotent when the account is not currently benched. 404 when the id is not the caller's **or** the row's `provider` does not match the path. Does not rewrite usage snapshots, tokens, priority, or labels — a still-exhausted usage window keeps the account unusable for routing until `resets_at`. The next bench-status upstream response re-benches it.

`PATCH /api/providers/:provider` sets the pool's routing strategy: body `{strategy}` — `ordered` is the only accepted value today, anything else is `400` ([providers.md](./providers.md) § Routing module). Upserts the `provider_settings` row ([database.md](./database.md)) and returns `{ok: true, strategy}`. The current value rides on `GET /api/providers/:provider/accounts` as a top-level `strategy` field (defaulting to `ordered` when no row exists) — no separate read route.

## Custom endpoint keys

Custom providers (BYO OpenAI-/Anthropic-compatible endpoint — see [providers.md](./providers.md)) are managed through a separate route group, not `/api/providers/:provider/*` — `:provider` there is gated to the builtin union and always 400s on a custom slug.

### Management routes (session required)

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/custom-providers` | List the user's custom providers; masked key + computed status, never the raw key. Each row also carries `account_id` — the id of the provider's single `upstream_accounts` row — so the Groups picker can pin a target to a custom endpoint's key like any other account ([admin-ui.md](./admin-ui.md) § Groups page); `null` if the account row is somehow missing |
| POST | `/api/custom-providers` | Create — body `{name, slug, format, base_url, api_key, count_tokens_url?, models_mode?, manual_models?}`; inserts the provider row plus one `upstream_accounts` row |
| PUT | `/api/custom-providers/:id` | Update — body `{name?, base_url?, api_key?, count_tokens_url?, models_mode?, manual_models?}`; `slug`/`format` are immutable (`400` if a differing value is sent); omitted or empty `api_key` keeps the stored key; a non-empty `api_key` re-encrypts and replaces it in place (same account row) |
| DELETE | `/api/custom-providers/:id` | Deletes the provider row and all its `upstream_accounts` rows (code-level cascade), then best-effort clears their bench keys |
| POST | `/api/custom-providers/:id/unpause` | Nulls the D1 bench columns on every `upstream_accounts` row of that endpoint (one key today). `{ok: true}`. Idempotent when none are benched. 404 if the id is not the caller's. Same contract as the builtin unpause: does not touch the stored key, and the next bench-status upstream response re-benches |
| PUT | `/api/custom-providers/order` | Reorder for display — body `{ids: string[]}` listing **every** one of the user's custom provider ids exactly once, in the desired order; renumbers `sort_order` densely in one transaction. `400` on a missing/extra/duplicate/foreign id (no partial write). Display only — routing is unaffected |
| POST | `/api/custom-providers/test` | Connectivity probe — body either `{format, base_url, api_key}` (pre-save) or `{id, base_url?}` (saved provider, uses its stored key); always `200` with `{ok, ...}` — see [providers.md](./providers.md) for the outcome mapping |

`count_tokens_url` (contract: [providers.md](./providers.md) § Custom endpoints) is validated by the same URL guard as `base_url` and accepted **only when `format` is `openai`** — sending a non-empty value on an anthropic-format provider is `400`, because that format already derives the endpoint from its base. Since it is nullable and the stored value *is* returned on read (it holds no secret), the update convention differs from `api_key`'s blank-means-keep: omitting the field keeps the stored value, and sending `""` or `null` **clears** it — the field's only way back to "unsupported". The list/create/update responses all carry `count_tokens_url` (`null` when unset).

The API key is **never** echoed back on any of these routes — `GET`/`POST`/`PUT` responses carry only `key_mask` (first 6 + `…` + last 4 of the plaintext key). Same session-cookie auth as `/api/providers/*`; same origin-locked CORS (see below).

## Model groups

Bare-name model aliases → ordered `provider/model` targets (contract: [providers.md](./providers.md) § Model groups; UI: [admin-ui.md](./admin-ui.md) § Groups page). Same session-cookie auth and origin-locked CORS as every `/api/*` route.

### Management routes (session required)

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/model-groups` | List the user's groups: `{id, name, aliases, targets, strategy, routing, created_at, updated_at}` each. `aliases` is the group's callable bare model ids (string array); `targets` is the priority-ordered array of `{model, account_id, account_label}` — `account_id` `null` for an unpinned (whole-pool) target; `account_label` is resolved at read time for display (`custom_label` \|\| upstream `label`, `null` when unpinned or the account no longer exists) and is **never** stored. `routing` is the current-route indicator ([providers.md](./providers.md) § Routing module): `{current_target_index, targets: [{usable, reason, unusable_until}]}`, aligned by index with `targets`. Computed at read time from **stored state only** (D1 bench columns + usage snapshots — the same facts dispatch uses; no upstream calls). `current_target_index` is the target the ordered walk would dispatch right now (`null` when none is usable); per-target `reason` is `null` when usable, else `"benched"` \| `"limit"` (bench wins when both apply and expires later) \| `"unresolved"` (prefix no longer resolves) \| `"no_account"` (pinned account gone, or unpinned pool empty); `unusable_until` is an ISO timestamp or `null` when unknown. Unpinned targets are usable when the provider pool has ≥1 usable account |
| POST | `/api/model-groups` | Create — body `{name, aliases, targets, strategy?}` (`strategy` defaults to `ordered`; only `ordered` accepted today — [providers.md](./providers.md) § Routing module). Each target is `{model, account_id?}` or a bare `"provider/model"` string (shorthand for `{model}`). Validation: `name` trimmed, 1–64 chars, free text, unique per user; `aliases` 1–10 entries, each trimmed, 1–128 chars, no whitespace, no `/`, no duplicates in the payload, unique across **all** of the caller's groups (`400` naming the conflicting alias); `targets` 1–20 entries, each `model` parses as `provider/model` with a prefix that is a builtin or one of the caller's custom slugs; `account_id`, when present, must be an `upstream_accounts` row owned by the caller whose `provider` matches the target's prefix; no duplicate `model`+`account_id` pairs; max 50 groups per user. `400` with a field-level message on any violation |
| PUT | `/api/model-groups/:id` | Update — body `{name?, aliases?, targets?, strategy?}`; `aliases` and `targets`, when present, each replace their whole list (no per-entry patching — order is the semantics for targets). Same validation as create. 404 when the id is not the caller's |
| DELETE | `/api/model-groups/:id` | Delete (aliases cascade). Requests already in flight finish; the next request for any of its aliases is `invalid_model` |

## Encryption

- `TOKEN_ENCRYPTION_KEY`: 32-byte key (base64) for AES-GCM of refresh/access tokens in D1.
- Never return raw upstream tokens to the browser.
