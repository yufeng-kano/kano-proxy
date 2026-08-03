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

### Management routes (session required)

| Method | Path |
|--------|------|
| GET | `/api/keys` |
| POST | `/api/keys` |
| DELETE | `/api/keys/:id` |
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

- **PKCE S256 required**, plus CLI flags (`id_token_add_organizations`, `codex_cli_simplified_flow`, `originator=codex_cli_rs`).
- Public OpenAI client only accepts registered redirect  
  `http://localhost:1455/auth/callback` (do **not** substitute kano domain).
- Browser lands on that URL (connection may fail if nothing listens) — user pastes **full URL** or `code#state` into admin UI to complete.

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
| DELETE | `/api/providers/:provider/accounts/:id` |
| POST | `/api/providers/:provider/accounts/import` |
| GET | `/api/providers/:provider/usage?refresh=` |

`:provider` ∈ `claude-code` | `codex` | `grok`. `accounts/import` is a manual credential-ingest route (bootstrapping / tests) — same shape as a completed OAuth login, but the caller supplies `access_token` (and optional `refresh_token` / `expires_at` / `account_id` / `email` / `label`) directly instead of running the OAuth dance.

## Custom endpoint keys

Custom providers (BYO OpenAI-/Anthropic-compatible endpoint — see [providers.md](./providers.md)) are managed through a separate route group, not `/api/providers/:provider/*` — `:provider` there is gated to the builtin union and always 400s on a custom slug.

### Management routes (session required)

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/custom-providers` | List the user's custom providers; masked key + computed status, never the raw key |
| POST | `/api/custom-providers` | Create — body `{name, slug, format, base_url, api_key, models_mode?, manual_models?}`; inserts the provider row plus one `upstream_accounts` row |
| PUT | `/api/custom-providers/:id` | Update — body `{name?, base_url?, api_key?, models_mode?, manual_models?}`; `slug`/`format` are immutable (`400` if a differing value is sent); omitted or empty `api_key` keeps the stored key; a non-empty `api_key` re-encrypts and replaces it in place (same account row) |
| DELETE | `/api/custom-providers/:id` | Deletes the provider row and all its `upstream_accounts` rows (code-level cascade), then best-effort clears their bench keys |
| POST | `/api/custom-providers/test` | Connectivity probe — body either `{format, base_url, api_key}` (pre-save) or `{id, base_url?}` (saved provider, uses its stored key); always `200` with `{ok, ...}` — see [providers.md](./providers.md) for the outcome mapping |

The API key is **never** echoed back on any of these routes — `GET`/`POST`/`PUT` responses carry only `key_mask` (first 6 + `…` + last 4 of the plaintext key). Same session-cookie auth as `/api/providers/*`; same origin-locked CORS (see below).

## Encryption

- `TOKEN_ENCRYPTION_KEY`: 32-byte key (base64) for AES-GCM of refresh/access tokens in D1.
- Never return raw upstream tokens to the browser.
