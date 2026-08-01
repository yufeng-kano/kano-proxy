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

### Private operator data (not in git)

Real production hostname, DNS tables, route bind order, and bootstrap scratch notes are **not** stored in this open docs tree. Use the gitignored local agent folder:

```bash
cp -R .local.example .local
# edit .local/dns.md, .local/deploy-notes.md, …
```

| Path | Tracked? | Contents |
|------|----------|----------|
| `.local.example/` | yes | Placeholder templates |
| `.local/` | **no** (gitignored) | Your real DNS, host, deploy checklist |

Agents and humans should read `.local/` when present; never copy live values from it into commits or public docs. Secrets (OAuth client secret, session keys, etc.) still go in `.dev.vars` / `wrangler secret`, not only as notes in `.local/`.

### DNS (Cloudflare)

| Type | Name | Target | Proxy |
|------|------|--------|-------|
| CNAME or A/AAAA | `<subdomain>` (or apex) | Pages/Workers as per CF attach flow | Proxied (orange cloud) |

Record exact bind order when attaching custom domain in dashboard; prefer Worker routes + Pages project on same zone. **Write the filled-in table to `.local/dns.md`**, not into this file.

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

**Do not put real production ids into the committed `apps/api/wrangler.toml`** (open-source placeholders only). Copy the production template and fill ids there:

```bash
cd apps/api
cp wrangler.production.example.toml wrangler.production.toml
# paste D1 database_id + KV ids; set APP_URL / GOOGLE_REDIRECT_URI to your domain
```

`wrangler.production.toml` is gitignored. Resource titles use the full `kano-proxy` prefix; Worker script name and D1 `database_name` are both `kano-proxy`. Optional: also mirror ids in gitignored `.local/deploy-notes.md`.

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

Always pass the **production** config so public `wrangler.toml` placeholders are not used against prod:

```bash
pnpm test
pnpm --filter api typecheck
cd apps/api
pnpm exec wrangler d1 migrations apply kano-proxy --remote --config wrangler.production.toml
pnpm exec wrangler deploy --config wrangler.production.toml
# secrets (values from .dev.vars or a password manager — never commit):
#   printf %s "$VAL" | pnpm exec wrangler secret put NAME --config wrangler.production.toml
```

Local dev keeps using `wrangler.toml` + `.dev.vars` (`pnpm --filter api dev`).

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

## CI / GitHub Actions (release deploy)

Production updates run when a **GitHub Release is published** (workflow: `.github/workflows/release-deploy.yml`). Pushing to `main` alone does **not** deploy.

### Version tags

| Policy | Rule |
|--------|------|
| Format | SemVer tag `vMAJOR.MINOR.PATCH` (e.g. `v1.2.0`) |
| Canonical package version | Root `package.json` → `"version": "MAJOR.MINOR.PATCH"` (no leading `v`) |
| **Default next release** | **Minor bump** → `x.(y+1).0` (patch resets to `0`) |
| Major | Breaking changes only, when intentional |
| Patch | Fix-only when you explicitly want `x.y.(z+1)` |

Example: last release `v0.3.1` → default next tag `v0.4.0` and `"version": "0.4.0"` in root `package.json`.

### Cutting a release (required steps)

A version bump is incomplete unless **all** of these land together:

1. **Bump root `package.json` `"version"`** to the new SemVer (e.g. `1.0.1`).
2. **Commit** that change with the release work (and any code/docs for the release).
3. **Push** the commit to `origin` (`main` or the release branch).
4. **Tag** `vMAJOR.MINOR.PATCH` on that commit and **publish a GitHub Release** (tag alone without a Release does not run deploy CI).

Do **not** create a GitHub Release / tag without updating and pushing `package.json` first. Keep tag and `package.json` version in lockstep (`v1.0.1` ↔ `"1.0.1"`).

```bash
# example: patch 1.0.0 → 1.0.1 after work is ready on main
# 1) set "version": "1.0.1" in package.json
git add package.json  # + other release files
git commit -m "Release v1.0.1: <summary>."
git push origin main

git tag -a v1.0.1 -m "v1.0.1"
git push origin v1.0.1
gh release create v1.0.1 --generate-notes
```

### Repository secrets (Settings → Secrets and variables → Actions)

| Secret | Purpose |
|--------|---------|
| `CLOUDFLARE_API_TOKEN` | Deploy Worker/Pages, apply D1 migrations |
| `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account id |
| `CF_D1_DATABASE_ID` | Production D1 `database_id` |
| `CF_KV_BENCH_ID` | Production KV id for `BENCH` |
| `CF_KV_CACHE_ID` | Production KV id for `CACHE` |

### Repository variables

| Variable | Purpose |
|----------|---------|
| `APP_URL` | Production origin, e.g. `https://<your-domain>` (no trailing slash). Workflow sets `GOOGLE_REDIRECT_URI` to `$APP_URL/api/auth/callback`. |

Worker secrets (`GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`, `SESSION_SECRET`, `TOKEN_ENCRYPTION_KEY`) are **not** set by CI on each release; configure once with `wrangler secret put --config wrangler.production.toml`.

### What the workflow does

1. Checkout release commit  
2. `pnpm install` → test → typecheck → web build  
3. Write ephemeral `apps/api/wrangler.production.toml` from secrets/vars (not committed)  
4. `wrangler d1 migrations apply --remote`  
5. `wrangler deploy` (Worker)  
6. `wrangler pages deploy` (project `kano-proxy`)

Manual re-deploy of a ref: Actions → **Release deploy** → Run workflow.