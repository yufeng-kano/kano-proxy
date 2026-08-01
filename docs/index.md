# kano-proxy documentation

Multi-tenant **subscription account-pool** proxy: OpenAI-compatible and Anthropic Messages APIs on Cloudflare, with per-user OAuth pools for Claude Code / Codex / Grok.

## Docs map

| Doc | Summary |
|-----|---------|
| [product.md](./product.md) | Goals, non-goals, tenants, providers, model naming |
| [api.md](./api.md) | Public LLM routes (`/openai/v1`, `/anthropic`), errors, reasoning, cache |
| [auth.md](./auth.md) | Google OIDC admin, client API keys, OAuth account binding |
| [database.md](./database.md) | D1 schema, secrets handling, migrations |
| [providers.md](./providers.md) | Per-provider pools, failover, usage windows, adapters |
| [admin-ui.md](./admin-ui.md) | Web UI pages and cache-first metadata UX |
| [project-structure.md](./project-structure.md) | Monorepo layout and module boundaries |
| [deployment.md](./deployment.md) | Domains, DNS, Wrangler, secrets, local dev, release CI |
| [logging.md](./logging.md) | What is logged (no content) |
| [testing.md](./testing.md) | Test strategy and commands |

## Stack (fixed)

- **API / proxy:** Cloudflare Workers (TypeScript)
- **Web:** Vue 3 + Vite + TypeScript on Cloudflare Pages
- **Data:** D1 (relational), KV (rate-limit / bench / short cache), Durable Objects only if pool coordination requires it
- **Envs:** local + production only

## Product one-liner

Users sign in with Google, bind their own subscription accounts (Claude Code / Codex / Grok), issue API keys, and call:

- `https://<your-domain>/openai/v1` — Chat Completions + Models  
- `https://<your-domain>/anthropic` — Messages API (`/v1/messages`)

Model ids use OpenRouter style: `provider/model` (e.g. `claude-code/claude-opus-5`). Set `<your-domain>` via DNS + Worker/Pages custom domain (see [deployment.md](./deployment.md)).
