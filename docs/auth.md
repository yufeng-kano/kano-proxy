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
| GET | `/api/usage/summary` | Aggregated `request_logs` for the dashboard; `?days=` 1, 7, or 30 (default 7) |

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
| POST | `/api/providers/:provider/login` |
| POST | `/api/providers/:provider/login/:id/complete` |
| GET | `/api/providers/:provider/login/:id` |
| POST | `/api/providers/:provider/accounts/:id/promote` |
| PATCH | `/api/providers/:provider/accounts/:id` |
| DELETE | `/api/providers/:provider/accounts/:id` |
| POST | `/api/providers/:provider/accounts/import` |
| GET | `/api/providers/:provider/usage?refresh=` |

`:provider` ∈ `claude-code` | `codex` | `grok`. `accounts/import` is a manual credential-ingest route (bootstrapping / tests) — same shape as a completed OAuth login, but the caller supplies `access_token` (and optional `refresh_token` / `expires_at` / `account_id` / `email` / `label`) directly instead of running the OAuth dance.

`PATCH /api/providers/:provider/accounts/:id` renames an account: body `{custom_label: string | null}`, trimmed, max 64 chars, `null`/`""` clears it and falls back to the upstream identity. It touches **only** `custom_label` — never tokens, priority, or the upstream-synced `label` (see [database.md](./database.md)) — and returns `{ok: true, custom_label}`. 404 when the id is not the caller's.

## Custom endpoint keys

Custom providers (BYO OpenAI-/Anthropic-compatible endpoint — see [providers.md](./providers.md)) are managed through a separate route group, not `/api/providers/:provider/*` — `:provider` there is gated to the builtin union and always 400s on a custom slug.

### Management routes (session required)

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/custom-providers` | List the user's custom providers; masked key + computed status, never the raw key. Each row also carries `account_id` — the id of the provider's single `upstream_accounts` row — so the Groups picker can pin a target to a custom endpoint's key like any other account ([admin-ui.md](./admin-ui.md) § Groups page); `null` if the account row is somehow missing |
| POST | `/api/custom-providers` | Create — body `{name, slug, format, base_url, api_key, models_mode?, manual_models?}`; inserts the provider row plus one `upstream_accounts` row |
| PUT | `/api/custom-providers/:id` | Update — body `{name?, base_url?, api_key?, models_mode?, manual_models?}`; `slug`/`format` are immutable (`400` if a differing value is sent); omitted or empty `api_key` keeps the stored key; a non-empty `api_key` re-encrypts and replaces it in place (same account row) |
| DELETE | `/api/custom-providers/:id` | Deletes the provider row and all its `upstream_accounts` rows (code-level cascade), then best-effort clears their bench keys |
| PUT | `/api/custom-providers/order` | Reorder for display — body `{ids: string[]}` listing **every** one of the user's custom provider ids exactly once, in the desired order; renumbers `sort_order` densely in one transaction. `400` on a missing/extra/duplicate/foreign id (no partial write). Display only — routing is unaffected |
| POST | `/api/custom-providers/test` | Connectivity probe — body either `{format, base_url, api_key}` (pre-save) or `{id, base_url?}` (saved provider, uses its stored key); always `200` with `{ok, ...}` — see [providers.md](./providers.md) for the outcome mapping |

The API key is **never** echoed back on any of these routes — `GET`/`POST`/`PUT` responses carry only `key_mask` (first 6 + `…` + last 4 of the plaintext key). Same session-cookie auth as `/api/providers/*`; same origin-locked CORS (see below).

## Model groups

Bare-name model aliases → ordered `provider/model` targets (contract: [providers.md](./providers.md) § Model groups; UI: [admin-ui.md](./admin-ui.md) § Groups page). Same session-cookie auth and origin-locked CORS as every `/api/*` route.

### Management routes (session required)

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/model-groups` | List the user's groups: `{id, name, targets, created_at, updated_at}` each. `targets` is the priority-ordered array of `{model, account_id, account_label}` — `account_id` `null` for an unpinned (whole-pool) target; `account_label` is resolved at read time for display (`custom_label` \|\| upstream `label`, `null` when unpinned or the account no longer exists) and is **never** stored |
| POST | `/api/model-groups` | Create — body `{name, targets}`. Each target is `{model, account_id?}` or a bare `"provider/model"` string (shorthand for `{model}`). Validation: name trimmed, 1–128 chars, no whitespace, no `/`, unique per user; `targets` 1–20 entries, each `model` parses as `provider/model` with a prefix that is a builtin or one of the caller's custom slugs; `account_id`, when present, must be an `upstream_accounts` row owned by the caller whose `provider` matches the target's prefix; no duplicate `model`+`account_id` pairs; max 50 groups per user. `400` with a field-level message on any violation |
| PUT | `/api/model-groups/:id` | Update — body `{name?, targets?}`; `name` **is** renameable (unlike a custom provider slug); `targets`, when present, replaces the whole ordered list (no per-entry patching — the order is the semantics). Same validation as create. 404 when the id is not the caller's |
| DELETE | `/api/model-groups/:id` | Delete. Requests already in flight finish; the next request for that name is `invalid_model` |

## Encryption

- `TOKEN_ENCRYPTION_KEY`: 32-byte key (base64) for AES-GCM of refresh/access tokens in D1.
- Never return raw upstream tokens to the browser.
