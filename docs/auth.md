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
| GET | `/api/auth/callback` | Exchange code, create session |
| POST | `/api/auth/logout` | Clear session |
| GET | `/api/auth/me` | Current user profile |

Anyone with a Google account may register on first login (insert user row).

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

## Upstream OAuth (account binding)

Secrets for public OAuth client ids may use well-known CLI defaults (override via env). Refresh tokens encrypted at rest with `TOKEN_ENCRYPTION_KEY`.

### Claude Code

- **PKCE S256 required** (`code_challenge` / `code_verifier`) — same as lincy `claude_code_proxy`.
- Browser authorize → user pastes `code#state` from Anthropic callback page.
- Token exchange must send `code_verifier`.
- Store access + refresh; multi-account pool.

### Codex

- **PKCE S256 required**, plus CLI flags (`id_token_add_organizations`, `codex_cli_simplified_flow`, `originator=codex_cli_rs`) — same as lincy `codex_proxy`.
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
| GET | `/api/providers/:provider/usage?refresh=` |

`:provider` ∈ `claude-code` | `codex` | `grok`.

## Encryption

- `TOKEN_ENCRYPTION_KEY`: 32-byte key (base64) for AES-GCM of refresh/access tokens in D1.
- Never return raw upstream tokens to the browser.
