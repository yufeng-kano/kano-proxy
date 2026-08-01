# Deployment

## Domains

Pick any hostname you control (example: `proxy.example.com`). Same host for UI + API is recommended.

| Host | Role |
|------|------|
| `https://<your-domain>` | Pages (UI) + Worker routes for `/openai/*`, `/anthropic/*`, `/api/*` |

Public LLM bases and admin “copy base URL” use the **request / browser origin** — no domain is hard-coded in app source. After deploy, set production vars to match:

```text
APP_URL=https://<your-domain>
GOOGLE_REDIRECT_URI=https://<your-domain>/api/auth/callback
```

### DNS (Cloudflare)

| Type | Name | Target | Proxy |
|------|------|--------|-------|
| CNAME or A/AAAA | `<subdomain>` (or apex) | Pages/Workers as per CF attach flow | Proxied (orange cloud) |

Record exact bind order when attaching custom domain in dashboard; prefer Worker routes + Pages project on same zone.

Suggested Worker routes (replace host):

- `<your-domain>/openai/*`
- `<your-domain>/anthropic/*`
- `<your-domain>/api/*`

Pages serves remaining paths (SPA).

## Production deploy

### Resources (once)

```bash
cd apps/api
npx wrangler login   # if needed
npx wrangler d1 create kano-proxy
npx wrangler kv namespace create kano-proxy-bench
npx wrangler kv namespace create kano-proxy-cache
```

Paste the printed D1 `database_id` and KV `id`s into `apps/api/wrangler.toml` (replace local placeholders). Resource titles use the full `kano-proxy` prefix; Worker script name and D1 `database_name` are both `kano-proxy`.

### Vars vs secrets

Public vars (Dashboard or wrangler production vars) — **not** the local defaults in `wrangler.toml`:

```text
APP_URL=https://<your-domain>
GOOGLE_REDIRECT_URI=https://<your-domain>/api/auth/callback
```

Secrets via `wrangler secret put` (never commit):

```text
GOOGLE_CLIENT_ID
GOOGLE_CLIENT_SECRET
SESSION_SECRET
TOKEN_ENCRYPTION_KEY
```

Optional overrides:

```text
CLAUDE_CODE_OAUTH_CLIENT_ID
CODEX_OAUTH_CLIENT_ID
GROK_OAUTH_CLIENT_ID
```

Google Cloud Console: authorize `https://<your-domain>/api/auth/callback`.

### Migrate + Worker

```bash
pnpm test
pnpm --filter api typecheck
cd apps/api && pnpm db:migrate:remote && pnpm deploy
```

### Pages (admin UI)

```bash
pnpm --filter web build
npx wrangler pages deploy apps/web/dist --project-name=kano-proxy
```

Production builds leave `VITE_API_ORIGIN` unset (same-origin to Worker routes on the same host).

#### Web env files

`vite build` runs in production mode and loads `apps/web/.env.production`. All four files are in `apps/web/`:

| File | Loaded by | Committed |
|------|-----------|-----------|
| `.env.example` | nothing — template only | yes |
| `.env.development` | `pnpm --filter web dev` | yes |
| `.env.production` | `pnpm --filter web build` | yes |
| `.env.*.local` | matching mode, overrides the above | **no** (gitignored) |

| Var | Dev | Production |
|-----|-----|------------|
| `VITE_API_ORIGIN` | `http://127.0.0.1:8787` | unset (same-origin) |
| `VITE_CONTACT_EMAIL` | contact address in the login footer | same |

`VITE_*` values are **inlined into the client bundle** at build time and are therefore public. Never put a secret in one — secrets go in `apps/api/.dev.vars` or `wrangler secret put`.

A `VITE_*` variable set in the Cloudflare Pages build environment **overrides** the committed `.env.production` value, so per-deploy changes need no commit.

### DNS + routes

1. Pages custom domain: `<your-domain>`
2. Worker routes: `/openai/*`, `/anthropic/*`, `/api/*` (optional `/health`) on that host
3. DNS CNAME/A, Proxied

| Surface | URL |
|---------|-----|
| Admin UI | `https://<your-domain>/` |
| OpenAI | `https://<your-domain>/openai/v1` |
| Anthropic | `https://<your-domain>/anthropic` |

## Local development

```bash
# root
pnpm install

# D1 migrations
cd apps/api && pnpm db:migrate:local

# API
pnpm --filter api dev    # wrangler dev :8787

# Web
pnpm --filter web dev    # :5173
```

Use **`http://127.0.0.1:5173`** (not `localhost`) for the admin UI so the session cookie host matches OAuth (`127.0.0.1:8787`).  
`apps/web/.env.development` sets `VITE_API_ORIGIN=http://127.0.0.1:8787`.  
`APP_URL` in `wrangler.toml` is `http://127.0.0.1:5173` so post-login redirects land on the SPA, not the Worker root (which is not a UI).

`.dev.vars` in `apps/api/` (gitignored):

```text
GOOGLE_CLIENT_ID=
GOOGLE_CLIENT_SECRET=
SESSION_SECRET=dev-session-secret-change-me
TOKEN_ENCRYPTION_KEY=   # 32 bytes base64
GOOGLE_REDIRECT_URI=http://127.0.0.1:8787/api/auth/callback
```


## Verify before deploy

```bash
pnpm test
pnpm --filter api typecheck
pnpm --filter web build
# then migrate:remote + api deploy + pages deploy (see Production deploy)
```
