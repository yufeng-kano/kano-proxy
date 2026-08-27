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
| GET | `/api/usage/summary` | Aggregated `request_logs` for the dashboard; `?from=` & `?to=` ISO strings with `?grain=hour|day` and `?offset=` (bucket calendar, minutes east of UTC, ±840, default 0), or legacy `?days=` 1, 7, or 30 (default 7). `models[]` is per **(provider, model)** — no account/alias dimension (see [admin-ui.md](./admin-ui.md) § Series shape) |
| GET | `/api/logs` | Newest-first `request_logs` rows for the caller; `?limit=` (default 50, max 100), `?cursor=` opaque keyset over `(created_at, id)`, `?provider=` exact slug, `?errors=1` (`error_code` set or `status_code >= 400`). Returns `{rows, next_cursor}`; rows carry read-time `account_label` / `api_key_name` + `api_key_removed` (the `api_keys` id is resolved server-side and never returned — see [admin-ui.md](./admin-ui.md) § Logs page), derived `usage_type` (`oauth` = builtin provider, `api` = anything else), and read-time cost fill for `NULL` `cost` |

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

### Antigravity

Plain Google OAuth 2.0 authorization code flow with a **confidential** client. No PKCE — the client secret is required on the code exchange *and* on every refresh.

- Authorize `https://accounts.google.com/o/oauth2/v2/auth` with `response_type=code`, `access_type=offline`, `prompt=consent`, `state`.
- Token `https://oauth2.googleapis.com/token`, form-encoded, `client_id` + `client_secret` on both `authorization_code` and `refresh_token` grants. Google does **not** rotate the refresh token on this grant and omits it from the refresh response — keep the stored one rather than nulling it.
- Scopes: `cloud-platform`, `userinfo.email`, `userinfo.profile`, `cclog`, `experimentsandconfigs`.
- Identity: `GET https://www.googleapis.com/oauth2/v2/userinfo?alt=json` for the account label.
- Refresh goes through the same per-account CAS single-flight every other provider uses ([providers.md](./providers.md) § OAuth refresh single-flight); the token endpoint is never called straight from a request path.

**The redirect URI cannot be this proxy — verified, not assumed.** The client's only registered redirect is `http://localhost:51121/oauth-callback`. Probed 2026-08-22 with the same client id and an arbitrary `https://…` redirect, Google's authorize endpoint 302s to its error page with `invalid_request` and the text *"You can't sign in to this app because it doesn't comply with Google's OAuth 2.0 policy for keeping apps secure"*, naming the rejected `redirect_uri`. Registering our own is not possible either — the client belongs to Google, not to this project.

So the flow is authorize-then-paste, like Claude Code: the user approves, the browser lands on a localhost address nothing is serving and shows a connection error, and they paste that whole URL (or just its `code` value) back into the dialog. `state` is carried in the URL and checked against the stored `oauth_login_states` row; a **bare code carries no state**, so that paste form has no CSRF binding of its own and is accepted only because the login row it completes is already session-scoped and single-use. The admin dialog tells the user the error page is expected — otherwise it reads as a failed sign-in.

After the exchange, login resolves the account's CloudCode project once (`v1internal:loadCodeAssist`, falling back to `v1internal:onboardUser`) and stores the project and tier ids **inside the encrypted credential payload** — no new column, and nothing on the dispatch path pays for that lookup. A bootstrap failure does not lose the tokens the user just approved: the account is bound anyway and the adapter retries the bootstrap on first use.

**The credential pair is not in this repo, and the provider is off until an operator supplies one.** `ANTIGRAVITY_OAUTH_CLIENT_ID` and `ANTIGRAVITY_OAUTH_CLIENT_SECRET` have **no built-in default** — with either unset, `POST /api/providers/antigravity/login` answers `400` naming the two variables instead of starting a flow that cannot finish, and the refresh path declines rather than burning a refresh token on a call that must fail. This differs from the other three providers, whose well-known CLI client ids are plain public identifiers with no secret attached.

Two reasons, and the second is the load-bearing one:

1. **Committing an OAuth client secret is forbidden here** regardless of how public it already is (see the project rules). GitHub's own push protection blocks it too.
2. **The obvious pair is Google's, not ours.** The credential the Antigravity desktop app ships with (mirrored in CLIProxyAPI `internal/auth/antigravity/constants.go`) is extractable from a distributed binary, so it is not confidential in any meaningful sense — but it is still Google's credential, and using it to reach the CloudCode API from something that is not Antigravity is very likely outside Google's terms for that client and for the AI Pro / Ultra subscription. Making the operator paste it in is what puts that decision, and its consequences, with the person who owns the deployment.

The endpoints themselves are undocumented and may change or be closed without notice. An operator with their own registered Google OAuth client can use it instead — but note the redirect constraint above applies to whatever client is configured: a client of your own can register a redirect you control, in which case the paste step is still what this proxy implements, and a hosted-callback flow would be a separate change.

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

`:provider` ∈ `claude-code` | `codex` | `grok` | `antigravity`. `accounts/import` is a manual credential-ingest route (bootstrapping / tests) — same shape as a completed OAuth login, but the caller supplies `access_token` (and optional `refresh_token` / `expires_at` / `account_id` / `email` / `label`) directly instead of running the OAuth dance.

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

Virtual endpoints — a slug under `/g/`, plus per-group models each mapping a name to ordered `provider/model` targets (contract: [providers.md](./providers.md) § Model groups; UI: [admin-ui.md](./admin-ui.md) § Groups page). Same session-cookie auth and origin-locked CORS as every `/api/*` route.

### Management routes (session required)

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/model-groups` | List the user's groups: `{id, name, slug, strategy, models, created_at, updated_at}` each. `models` is the group's model list; each model is `{name, targets, routing}` where `targets` is the priority-ordered array of `{model, account_id, account_label}` — `account_id` `null` for an unpinned (whole-pool) target; `account_label` is resolved at read time for display (`custom_label` \|\| upstream `label`, `null` when unpinned or the account no longer exists) and is **never** stored. `routing` is that model's current-route indicator ([providers.md](./providers.md) § Routing module): `{current_target_index, targets: [{usable, reason, unusable_until}]}`, aligned by index with its `targets`. Computed at read time from **stored state only** (D1 bench columns + usage snapshots — the same facts dispatch uses; no upstream calls). `current_target_index` is the target the ordered walk would dispatch right now (`null` when none is usable); per-target `reason` is `null` when usable, else `"benched"` \| `"limit"` (bench wins when both apply and expires later) \| `"unresolved"` (prefix no longer resolves) \| `"no_account"` (pinned account gone, or unpinned pool empty); `unusable_until` is an ISO timestamp or `null` when unknown. Unpinned targets are usable when the provider pool has ≥1 usable account |
| POST | `/api/model-groups` | Create — body `{name, slug, models, strategy?}` (`strategy` defaults to `ordered`; only `ordered` accepted today — [providers.md](./providers.md) § Routing module). `models` is an array of `{name, targets}`; each target is `{model, account_id?}` or a bare `"provider/model"` string (shorthand for `{model}`). Validation: `name` trimmed, 1–64 chars, free text, unique per user; `slug` matches the custom-provider slug shape (`^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$`), unique across the caller's groups; `models` 1–20 entries, each `name` trimmed, 1–128 chars, no whitespace (`/` allowed), no duplicate names in the payload (group-scoped uniqueness — other groups may reuse a name); per model `targets` 1–20 entries, each `model` parses as `provider/model` with a prefix that is a builtin or one of the caller's custom slugs; `account_id`, when present, must be an `upstream_accounts` row owned by the caller whose `provider` matches the target's prefix; no duplicate `model`+`account_id` pairs within one model's list; max 50 groups per user. `400` with a field-level message on any violation |
| PUT | `/api/model-groups/:id` | Update — body `{name?, slug?, models?, strategy?}`; `models`, when present, replaces the whole set (no per-entry patching — order is the semantics for targets, and the model set is saved as one unit). Changing `slug` moves the endpoint URL immediately. Same validation as create. 404 when the id is not the caller's |
| DELETE | `/api/model-groups/:id` | Delete (model rows cascade). Requests already in flight finish; the next request to `/g/<slug>/…` is a 404 |

## Encryption

- `TOKEN_ENCRYPTION_KEY`: 32-byte key (base64) for AES-GCM of refresh/access tokens in D1.
- Never return raw upstream tokens to the browser.
