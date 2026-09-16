# Deployment

kano-proxy runs as four containers on a machine you control: PostgreSQL, the API server, the web app, and Caddy in front for TLS and routing. There is no managed platform and no external service in the request path.

## Quick start

```sh
git clone https://github.com/yufeng-kano/kano-proxy.git
cd kano-proxy
cp .env.example .env     # fill it in — see Configuration
docker compose up -d --build
```

Point `DOMAIN`'s DNS at the machine and open port 80 and 443 to it. Caddy gets a certificate on first request. The server applies its migrations at start, so an empty database is ready by the time it listens.

If you already run a reverse proxy, delete the `caddy` service and send `/api/*`, `/openai/*`, `/anthropic/*`, `/g/*`, `/agent/*` and `/health` to the server on `:8787` and everything else to the web app on `:80`. Do not buffer responses and allow long idle gaps: model streams and the agent tunnel are long-lived connections.

## Domains

One hostname serves both the UI and the API; that is the arrangement the app assumes. Public LLM bases and the admin "copy base URL" button use the request origin, so no domain is hard-coded anywhere in the source.

```text
APP_URL=https://<your-domain>
GOOGLE_REDIRECT_URI=https://<your-domain>/api/auth/callback
```

The same `<your-domain>/api/auth/callback` must be registered as an authorized redirect URI on the Google OAuth client.

### Private operator data (not in git)

Real hostnames, DNS tables and deploy notes do not belong in this open documentation tree. Use the gitignored local folder:

```sh
cp -R .local.example .local
# edit .local/dns.md, .local/deploy-notes.md, …
```

## Configuration

Everything is environment variables, read once at start. `.env.example` is the complete list; these are the ones you must set.

| Variable | Meaning |
|---|---|
| `DOMAIN`, `ACME_EMAIL` | What Caddy serves and who Let's Encrypt contacts |
| `APP_URL`, `GOOGLE_REDIRECT_URI` | The public origin and the OAuth callback on it |
| `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET` | The Google OAuth client that admin sign-in uses |
| `POSTGRES_USER`, `POSTGRES_PASSWORD`, `POSTGRES_DB` | The database the compose file creates and the server connects to |
| `SESSION_SECRET` | Signs session cookies. Changing it signs everyone out |
| `TOKEN_ENCRYPTION_KEY` | Encrypts stored upstream credentials. **Changing it makes every bound account permanently unreadable** |
| `CLI_TOKEN_SECRET` | Signs CLI access tokens ([cli.md](./cli.md)) |

Optional: provider OAuth client ids (unset uses the pinned defaults), `GITHUB_REPO` and `GITHUB_TOKEN` for the in-app changelog, `REQUEST_LOG_RETENTION_DAYS`, `UPSTREAM_FIRST_BYTE_TIMEOUT_MS`, `RUST_LOG`.

Generate the three secrets once and keep them somewhere you will not lose them:

```sh
openssl rand -hex 32     # SESSION_SECRET, CLI_TOKEN_SECRET
openssl rand -base64 32  # TOKEN_ENCRYPTION_KEY
```

## Database and migrations

The server applies the migrations in `apps/api/crates/kano-proxy-core/migrations/` in order at start and records what it applied in `core_migrations`. Restarting the same build applies nothing. Applied migrations are immutable: a schema change is a new file, never an edit to an old one ([database.md](./database.md)).

An edition built on this core keeps its own migration table and applies after the core.

The database lives in `./data/postgres` and the certificates in `./data/caddy`, both gitignored. Back those up; everything else is rebuildable from the repository.

## Local development

```sh
# the API, against a Postgres you already have
cd apps/api
DATABASE_URL=postgres://… GOOGLE_CLIENT_ID=… cargo run -p kano-proxy-api

# the web app, in another terminal — it proxies /api to 127.0.0.1:8787
cd apps/web && pnpm install && pnpm dev

# the documentation site
cd apps/docs && pnpm install && pnpm dev
```

Each app installs and builds where it lives; there is no repository-wide package manifest.

The server serves the built SPA itself when `WEB_DIST_DIR` points at it, which is useful for a single-process run. The compose stack does not use that: the web app is its own container so the two concerns cannot drift into one process.

## Verify before deploying

```sh
cd apps/api && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
cd apps/web    && pnpm test && pnpm typecheck && pnpm build
cd apps/docs   && pnpm build
cd apps/cli    && cargo test
```

Database-backed tests need `KANO_TEST_DATABASE_URL` pointing at a throwaway PostgreSQL and skip themselves without it. No test sends real upstream traffic ([testing.md](./testing.md)).

## Updating

```sh
git pull
docker compose up -d --build
```

The server migrates on the way up. Read the release notes before updating across a schema change, and take a database backup first.

## Releases

A published `vX.Y.Z` Release is the product's release. It does not deploy anything by itself: this repository builds and verifies, and operators update their own instances. Tags must match the `version` in `apps/api/crates/kano-proxy-core/Cargo.toml`, bumped, committed and pushed before tagging; the default bump is minor.

Release notes are hand-written and passed inline with `--notes`; never publish with `--generate-notes` alone. Those notes are the only thing the in-app `/changelog` page has to show, so write them for the operator, not for the diff.

### CLI release (`cli-vX.Y.Z`, workflow `cli-release.yml`)

The `kano-proxy` CLI ([cli.md](./cli.md) § Distribution) ships to end users' machines and has nothing to do with which server build is live, so it carries **its own SemVer line** — reset to `1.0.0` when it was decoupled from the product's 4.x — and its own tag prefix. Wire compatibility is the protocol's `proto` number, never a version comparison, so the two lines drift freely.

| Policy | Rule |
|--------|------|
| Tag | `cli-vMAJOR.MINOR.PATCH` (e.g. `cli-v1.2.0`) |
| Canonical version | `apps/cli/Cargo.toml` `version` (refresh the `kano-proxy` entry in `Cargo.lock` with `cargo update -p kano-proxy --offline` or a build) |
| Cadence | Only when the CLI changed. A server hotfix does not bump the CLI; a CLI fix does not deploy the server |
| `gh release create … --latest=false` | **Required.** GitHub's `releases/latest` must keep pointing at the product release — the `/changelog` page and anyone reading `latest` expect a `v…` tag there. The CLI's own channels never read `latest` (below) |

Steps, mirroring the product flow: bump `Cargo.toml` (+ `Cargo.lock`), commit, push, then

```sh
gh release create cli-v1.0.1 --title cli-v1.0.1 --latest=false --notes '<what changed for CLI users>'
```

On publish, `cli-release.yml`:

1. **`check`** — tag == `Cargo.toml` (fails the run before any build), then `cargo test`.
2. **`build`** matrix, `needs: check`: `aarch64-apple-darwin` + `x86_64-apple-darwin` (macOS runner), `x86_64-unknown-linux-musl` + `aarch64-unknown-linux-musl` (Linux runner, `cross`), `x86_64-pc-windows-msvc` (Windows runner). Packages `kano-proxy-<version>-<target>.tar.gz` (`.zip` for Windows).
3. **`publish`** — one `SHA256SUMS` over all archives, attached to the Release with the archives; then bumps the Homebrew formula (`yufeng-kano/homebrew-tap`) and Scoop manifest (`yufeng-kano/scoop-bucket`) to the new version + checksums with `TAP_PUSH_TOKEN` (skipped with a warning when the secret is absent; re-run the job after adding it).

**The assets land minutes after the Release is published**, and a failed build leaves a `cli-v…` Release with no assets at all. The install channels are built to tolerate both: `kano-proxy update` and `scripts/install-cli.sh` list recent releases and take the **newest `cli-v` release that actually carries this platform's archive**, so during the build window (or after a failed one) they keep serving the previous release instead of erroring. Homebrew/Scoop only move when the bump step runs, which is after the assets exist.

The release notes on a `cli-v` Release are for CLI users on GitHub; the `/changelog` page skips `cli-v` tags entirely ([changelog.md](./changelog.md)).

PR CI (`ci.yml`) compile-checks all five targets plus `cargo test`, so a PR cannot merge a CLI that does not build.
