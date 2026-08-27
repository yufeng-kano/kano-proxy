# kano-proxy documentation

Multi-tenant **subscription account-pool** proxy: OpenAI-compatible and Anthropic Messages APIs on Cloudflare, with per-user OAuth pools for Claude Code / Codex / Grok / Antigravity.

## Docs map

| Doc | Summary |
|-----|---------|
| [product.md](./product.md) | Goals, non-goals, tenants, providers, model naming |
| [api.md](./api.md) | Public LLM routes (`/openai/v1`, `/anthropic`), errors, reasoning, audio (input & transcriptions), cache |
| [auth.md](./auth.md) | Google OIDC admin, client API keys, OAuth account binding |
| [database.md](./database.md) | D1 schema, secrets handling, migrations |
| [providers.md](./providers.md) | Per-provider pools, routing module + strategies, failover, usage windows, manual unpause, adapters |
| [pricing.md](./pricing.md) | Estimated per-request cost (LiteLLM + OpenRouter tables), per-key spend limits |
| [codex-relay.md](./codex-relay.md) | **Approved exception to Cloudflare-only.** Codex egress relay on Cloud Run: design, IAM auth, cost, deploy |
| [admin-ui.md](./admin-ui.md) | Web UI: design restraint (the house style), shell layout, pages, responsive rules, cache-first UX |
| [i18n.md](./i18n.md) | Message catalog, translation runtime, copy voice |
| [changelog.md](./changelog.md) | Release notes from GitHub, running version, caching + sanitization |
| [project-structure.md](./project-structure.md) | Monorepo layout and module boundaries |
| [deployment.md](./deployment.md) | Domains, DNS, Wrangler, secrets, local dev, release CI |
| [logging.md](./logging.md) | What is logged (no content) |
| [testing.md](./testing.md) | Test strategy, cost-safety rules, commands |

## Stack (fixed)

- **API / proxy:** Cloudflare Workers (TypeScript)
- **Web:** Vue 3 + Vite + TypeScript on Cloudflare Pages
- **Data:** D1 (relational state), KV (rate-limit / short cache; deprecated BENCH binding retained for config compatibility), Durable Objects only if pool coordination requires it
- **Codex egress relay:** Deno container on Google Cloud Run (us-central1) — the single approved non-Cloudflare component; see [codex-relay.md](./codex-relay.md)
- **Envs:** local + production only

## Product one-liner

Users sign in with Google, bind their own subscription accounts (Claude Code / Codex / Grok / Antigravity) or bring their own OpenAI-/Anthropic-compatible endpoint (custom base URL + API key), issue API keys, and call **any bound provider** through either format:

- `https://<your-domain>/openai/v1` — Chat Completions, Models, and Audio Transcriptions  
- `https://<your-domain>/anthropic` — Messages API (`/v1/messages`)

Model ids use OpenRouter style on **both** bases: `provider/model` (e.g. `claude-code/claude-opus-5`, `grok/grok-4.5`, `antigravity/gemini-3-flash`, or `<your-slug>/<upstream-model>` for a custom endpoint). Each user-defined **model group** is its own virtual endpoint — `https://<your-domain>/g/<slug>/openai/v1` and `/g/<slug>/anthropic` — on which the group's configured model names expand to ordered lists of `provider/model` targets ([providers.md](./providers.md)). Set `<your-domain>` via DNS + Worker/Pages custom domain (see [deployment.md](./deployment.md)).
