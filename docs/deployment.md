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

Every Wrangler config (committed `wrangler.toml`, the production example, the CI-generated production config) carries the same `[triggers] crons` block for the daily retention sweep (see [logging.md](./logging.md)). If your `wrangler.production.toml` predates it, copy the `[triggers]` block from `wrangler.production.example.toml` — a deploy from a config without it silently drops the cron.

The same three configs also carry `[limits] cpu_ms = 15000` — a per-invocation CPU ceiling that bounds runaway-billing risk (an infinite-loop bug or abusive request errors out at 15s of CPU instead of billing the platform maximum). **Requires Workers Paid**: a Free-plan deploy rejects the `[limits]` block, so delete it if running this project on Free (Free enforces its own ~10ms budget, which long SSE streams exceed — see the observability note below). The value is sized from production measurements: the heaviest legitimate request observed (48s stream, ~5.4k output tokens) accrued 1.3s of CPU; an extrapolated worst case (~32k-token output) stays under ~8s; raise the ceiling if Workers Logs ever shows `exceededCpu` on legitimate traffic.

The same three configs also carry `[observability] enabled = true` (Workers Logs / invocation logs). This is the only place resource-limit kills are visible: a request killed for exceeding CPU/memory (Cloudflare error 1102, tail outcome `exceededCpu`/`exceededMemory`) loses its `waitUntil` work, so its `request_logs` row is never written — D1 shows a *gap*, not an error. Workers Logs records the invocation outcome platform-side, so those kills (and any other invisible failure) can be diagnosed after the fact instead of only while a `wrangler tail` happens to be attached. Query them in Dashboard → Workers → kano-proxy → Logs. Volume guard: unauthenticated 401 floods count too — investigate any client hammering the endpoints, since log events are the billable unit past the included allotment.

### Vars vs secrets

Public vars (Dashboard or wrangler production vars) — **not** the local defaults in `wrangler.toml`:

```text
APP_URL=https://<your-domain>
GOOGLE_REDIRECT_URI=https://<your-domain>/api/auth/callback
CODEX_RELAY_URL=https://<relay>.run.app   # optional — codex egress relay (docs/codex-relay.md); unset = relay off
```

Secrets via `wrangler secret put` (never commit):

```text
GOOGLE_CLIENT_ID
GOOGLE_CLIENT_SECRET
SESSION_SECRET
TOKEN_ENCRYPTION_KEY
CODEX_RELAY_SA_KEY   # optional — GCP SA JSON key for the codex relay (docs/codex-relay.md)
```

Optional overrides:

```text
CLAUDE_CODE_OAUTH_CLIENT_ID
CODEX_OAUTH_CLIENT_ID
GROK_OAUTH_CLIENT_ID
REQUEST_LOG_RETENTION_DAYS   # retention sweep window in days; default 90
GITHUB_TOKEN                 # optional; raises the /changelog GitHub rate limit (secret)
```

`GITHUB_REPO` (`owner/repo`, source of the `/changelog` release notes) is **public**, so it lives in `wrangler.toml` `[vars]` and is carried into production by the CI config writer — not in the secret store. `GITHUB_TOKEN` is optional: the KV cache keeps a deploy inside the unauthenticated 60/hr budget on its own. See [changelog.md](./changelog.md).

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
npx wrangler pages deploy apps/web/dist --project-name=kano-proxy --branch=main
```

`--branch` must equal the Pages project's **production branch** (`main`), or the upload becomes a Preview deployment and the production domain keeps serving the old build. This matters especially in CI, where a release checkout is a detached HEAD and wrangler would otherwise infer branch `HEAD`.

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

## Codex egress relay (Cloud Run)

Design and rationale: [codex-relay.md](./codex-relay.md). The relay is the one approved non-Cloudflare component. **Deploys are manual** — release CI never touches it. Real project id / region / service URL go in gitignored `.local/relay.md`; placeholders only here.

### GCP one-time setup

```bash
gcloud auth login
gcloud projects create <gcp-project-id>
gcloud billing projects link <gcp-project-id> --billing-account=<billing-account-id>
gcloud config set project <gcp-project-id>
gcloud services enable run.googleapis.com cloudbuild.googleapis.com artifactregistry.googleapis.com
gcloud iam service-accounts create kano-relay-invoker --display-name="kano-proxy relay invoker"
```

### Deploy / update the relay

```bash
cd apps/relay
deno task test && deno task check
gcloud run deploy kano-codex-relay --source . --region us-central1 \
  --no-allow-unauthenticated --timeout=3600 --min-instances=0 --max-instances=10 \
  --concurrency=1 --cpu=0.25 --memory=256Mi
gcloud run services add-iam-policy-binding kano-codex-relay --region=us-central1 \
  --member="serviceAccount:kano-relay-invoker@<gcp-project-id>.iam.gserviceaccount.com" \
  --role="roles/run.invoker"
```

(`--concurrency=1` is forced: Cloud Run rejects fractional CPU with concurrency > 1 (`Total cpu < 1 is not supported with concurrency > 1`, measured 2026-08-03), and 0.25 vCPU billing beats 1 vCPU shared — see [codex-relay.md](./codex-relay.md#cloud-run-configuration-and-cost). With instance-per-stream, `--max-instances` is the concurrent-codex-stream ceiling; requests past it get a marker-less 429 that the Worker guard converts to a non-benching 502.)

### Wire the Worker to it

```bash
# SA key → Cloudflare secret (never commit the JSON; delete the local file after)
gcloud iam service-accounts keys create relay-invoker.json \
  --iam-account=kano-relay-invoker@<gcp-project-id>.iam.gserviceaccount.com
cd apps/api
pnpm exec wrangler secret put CODEX_RELAY_SA_KEY --config wrangler.production.toml < relay-invoker.json
rm relay-invoker.json
```

- `CODEX_RELAY_URL` (the service's `https://….run.app` origin) goes in `wrangler.production.toml` `[vars]` **and** in the GitHub repository variable `CODEX_RELAY_URL` so release CI keeps it (see the CI section). Leave both unset to disable the relay — codex then 403s direct, as before the relay existed.
- Local dev (optional): put both values in `apps/api/.dev.vars` to exercise the relay from `wrangler dev`.
- Key rotation: create a new key, `wrangler secret put` again, delete the old key in GCP. No relay redeploy.

### Spike check (free)

After deploy, verify egress with a **deliberately fake** upstream token. Expected: `401` + JSON (the wall would be `403` + HTML). Sending `CF-Worker` manually proves the allowlist drops it:

```bash
curl -sS -D - -o /dev/null -X POST \
  -H "X-Serverless-Authorization: Bearer $(gcloud auth print-identity-token)" \
  -H "Authorization: Bearer fake-spike-token" \
  -H "content-type: application/json" \
  -H "CF-Worker: spike-test" -H "CF-Connecting-IP: 1.2.3.4" \
  "https://<relay>.run.app/backend-api/codex/responses" -d '{"model":"gpt-5"}'
```

Never spike with a real token or a real prompt (cost-safety rule in `CLAUDE.md`).

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

### Local D1 / KV persistence

Local data **does** persist across `wrangler dev` restarts. It is **not** the same as production D1.

| Rule | Why |
|------|-----|
| Always run API via `pnpm --filter api dev` / `pnpm --filter api db:migrate:local` | Scripts pin `--persist-to .wrangler/state` under `apps/api/`. Running bare `wrangler` from the monorepo root creates a **different** state tree. |
| Do **not** change `database_id` in committed `wrangler.toml` | Local Miniflare maps D1 to a durable-object-style sqlite file keyed off that id. Changing the placeholder creates a **new empty** local DB and leaves the old one orphaned under `.wrangler/state/v3/d1/`. |
| Keep `TOKEN_ENCRYPTION_KEY` stable in `.dev.vars` | Upstream OAuth blobs in `upstream_accounts.encrypted_payload` are AES-GCM with this key. Rotating it makes old local accounts decrypt-fail (UI looks empty / unusable). |
| Do **not** delete `apps/api/.wrangler/state/` unless you want a wipe | That directory is the local D1 + KV store (gitignored). |
| Prefer one wrangler dev at a time on this project | Concurrent dev processes can race local sqlite / metadata. |

Quick health check (counts on the active local DB):

```bash
pnpm --filter api db:local:status
```

If accounts “vanish” after a re-login but the Google user still works: check for **multiple** `*.sqlite` under `apps/api/.wrangler/state/v3/d1/miniflare-D1DatabaseObject/`. Only one file should hold your data; extras are usually orphans from an earlier `database_id` / state split. Back up, then either re-bind accounts or merge rows into the sqlite that `db:local:status` is reading (same path as `--persist-to`).

Local backups from recovery work may live under `apps/api/.wrangler/backups/` (also gitignored).

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
4. **Write the release notes** (see below) — they are a deliverable of the release, not a formality.
5. **Tag** `vMAJOR.MINOR.PATCH` on that commit and **publish a GitHub Release** (tag alone without a Release does not run deploy CI).

Do **not** create a GitHub Release / tag without updating and pushing `package.json` first. Keep tag and `package.json` version in lockstep (`v1.0.1` ↔ `"1.0.1"`).

#### Release notes are hand-written

**Never cut a release with `--generate-notes` alone.** That flag builds its output from *merged pull requests*; this repo lands work as direct commits to `main`, so it has nothing to summarize and emits a bare `**Full Changelog**: …compare/…` line. A release published that way is blank on the `/changelog` page — and those notes are the **only** source that page has (no `CHANGELOG.md`, no D1 table — see [changelog.md](./changelog.md)). v2.1.0–v2.2.1 shipped blank this way and were backfilled by hand.

Pass the notes inline with `gh release create --notes '<markdown>'` — no scratch file needed. What the notes are for:

- **Address the operator, in the product's voice** (the copy rules in [i18n.md](./i18n.md) § Copy voice apply): say what they can now do, not which module changed. The commit message is where the mechanism goes.
- Lead with a one-line summary of the release, then group under `##` headings when there is more than a handful of items.
- Only these tags survive sanitization: `a code em h2 h3 li p strong tt ul`. Tables and images degrade to plain text — do not reach for them.
- A fix-only release can be a few bullets; it still needs to name the fix in terms of the symptom the user saw.

```bash
# example: patch 1.0.0 → 1.0.1 after work is ready on main
# 1) set "version": "1.0.1" in package.json
git add package.json  # + other release files
git commit -m "Release v1.0.1: <summary>."
git push origin main

git tag -a v1.0.1 -m "v1.0.1"
git push origin v1.0.1

# 2) write the user-facing notes inline, then publish
gh release create v1.0.1 --title v1.0.1 --notes '
A one-line summary of the release.

## What's new
- First change users can see.
- Second change users can see.
'
```

Notes can be corrected after the fact with `gh release edit <tag> --notes '<markdown>'`; editing a release does **not** re-run the deploy workflow (it triggers on `published`, not `edited`). The `/changelog` page refetches within an hour, or immediately via its Refresh.

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
| `CODEX_RELAY_URL` | **Optional.** Codex egress relay origin (`https://….run.app`, [codex-relay.md](./codex-relay.md)). Empty/unset omits the var from the generated config (relay off). |

Worker secrets (`GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`, `SESSION_SECRET`, `TOKEN_ENCRYPTION_KEY`) are **not** set by CI on each release; configure once with `wrangler secret put --config wrangler.production.toml`.

### What the workflow does

1. Checkout release commit  
2. `pnpm install` → test → typecheck → web build  
3. Write ephemeral `apps/api/wrangler.production.toml` from secrets/vars (not committed)  
4. `wrangler d1 migrations apply --remote`  
5. `wrangler deploy` (Worker)  
6. `wrangler pages deploy` (project `kano-proxy`)

Manual re-deploy of a ref: Actions → **Release deploy** → Run workflow.