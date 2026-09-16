# kano-proxy documentation

Multi-tenant **subscription account-pool** proxy: OpenAI-compatible and Anthropic Messages APIs, with per-user OAuth pools for Claude Code / Codex / Grok / Antigravity. Self-hosted: one Rust server and a PostgreSQL database, run with docker compose. The Cloudflare Workers edition this replaced is kept on the `serverless` branch.

## Docs map

| Doc | Summary |
|-----|---------|
| [rust-server.md](./rust-server.md) | The server: rationale for Rust, crate layout, Postgres storage, configuration and the compatibility contract it had to keep |
| [cloud-edition.md](./cloud-edition.md) | Public core/private kano-proxy-cloud boundaries, subscription contracts, verification and cutover gates |
| [product.md](./product.md) | Goals, non-goals, tenants, providers, model naming |
| [api.md](./api.md) | Public LLM routes (`/openai/v1` Chat Completions + Responses API, `/anthropic`), errors, reasoning, audio (input & transcriptions), cache |
| [auth.md](./auth.md) | Google OIDC admin, client API keys, OAuth account binding |
| [database.md](./database.md) | Schema, secrets handling, migrations |
| [providers.md](./providers.md) | Per-provider pools, routing module + strategies, failover, usage windows, manual unpause, adapters, turn-scoped Codex reasoning replay |
| [pricing.md](./pricing.md) | Estimated per-request cost (LiteLLM + OpenRouter tables), per-key spend limits |
| [cli.md](./cli.md) | CLI providers: the official `kano-proxy` CLI (device login, Rust/ratatui) + the reverse tunnel exposing local LLMs as first-class providers |
| [admin-ui.md](./admin-ui.md) | Web UI: design restraint (the house style), shell layout, pages, responsive rules, cache-first UX |
| [docs-site.md](./docs-site.md) | Public `/docs/` site (VitePress, en + zh-TW): build, same-host serving, origin fill, agent-page rules, SEO and indexing of the whole host |
| [i18n.md](./i18n.md) | Message catalog, translation runtime, copy voice |
| [changelog.md](./changelog.md) | Release notes from GitHub, running version, caching + sanitization |
| [project-structure.md](./project-structure.md) | Monorepo layout and module boundaries |
| [deployment.md](./deployment.md) | Running the server: configuration, domains, database, local dev and releases |
| [logging.md](./logging.md) | What is logged (no content) |
| [testing.md](./testing.md) | Test strategy, cost-safety rules, commands |

## Stack (fixed)

- **Server:** Rust (axum, tokio, sqlx), one process, direct upstream egress
- **Web:** Vue 3 + Vite + TypeScript, built to static files the server or any web server can serve
- **Data:** PostgreSQL. In-process caches replace what a key-value store used to hold; the tunnel registry lives in the process
- **Envs:** local + production only

Each app owns its own manifest, lockfile and build; nothing language-specific sits at the repository root, and the release version is the `version` of the `kano-proxy-core` crate (`apps/api/crates/kano-proxy-core/Cargo.toml`).

## Product one-liner

Users sign in with Google, bind their own subscription accounts (Claude Code / Codex / Grok / Antigravity) or bring their own OpenAI-/Anthropic-compatible endpoint (custom base URL + API key), issue API keys, and call **any bound provider** through either format:

- `https://<your-domain>/openai/v1` — Chat Completions, Responses (the Codex CLI's wire), Models, and Audio Transcriptions  
- `https://<your-domain>/anthropic` — Messages API (`/v1/messages`)

Model ids use OpenRouter style on **both** bases: `provider/model` (e.g. `claude-code/claude-opus-5`, `grok/grok-4.5`, `antigravity/gemini-3-flash`, or `<your-slug>/<upstream-model>` for a custom endpoint). Each user-defined **model group** is its own virtual endpoint — `https://<your-domain>/g/<slug>/openai/v1` and `/g/<slug>/anthropic` — on which the group's configured model names expand to ordered lists of `provider/model` targets ([providers.md](./providers.md)). Point `<your-domain>` at your server and give it a certificate (see [deployment.md](./deployment.md)).
