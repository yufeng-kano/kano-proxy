# Kano Proxy

**Turn your own AI subscriptions into OpenAI- and Anthropic-compatible APIs.**

Kano Proxy is a multi-tenant account-pool proxy: sign in, bind Claude Code / Codex / Grok accounts, issue API keys, and point coding agents or SDKs at a single base URL. Your upstream OAuth tokens stay server-side; clients only ever use project-issued keys.

---

## Why Kano Proxy

| Pain | What you get |
|------|----------------|
| Coding tools want an OpenAI or Anthropic base URL, but you pay for subscriptions | OpenAI-compatible + Anthropic Messages endpoints on one host |
| Multiple accounts, rate limits, and 429s | Per-user multi-account pools with acquire → failover → bench |
| Sharing Platform API keys is costly or off-limits | Uses **your** subscription OAuth pools — not aggregated Platform keys |
| Hard to see who’s left on quota | Admin UI with 5h / weekly usage bars per account |
| Prompts must not leak into logs | No full prompt/completion logging by default |

---

## How it works

```text
  Coding agent / SDK          Kano Proxy                 Your subscriptions
  ─────────────────      ─────────────────────      ─────────────────────────
  base URL + API key  →  auth + pool routing    →  Claude Code / Codex / Grok
  model: provider/…   →  stream end-to-end      →  OAuth tokens never leave
                         usage & keys in UI         the Worker
```

1. **Sign in** with Google (admin UI).
2. **Bind** one or more subscription accounts per provider.
3. **Create** an API key (`sk-kano-proxy-…`).
4. **Configure** your client with the base URL, key, and a `provider/model` id.

Each user’s accounts and keys are isolated — User A’s pool never serves User B.

---

## Use it as a client

Replace `<your-domain>` with your deployment hostname.

| Protocol | Base URL |
|----------|----------|
| OpenAI-compatible | `https://<your-domain>/openai/v1` |
| Anthropic Messages | `https://<your-domain>/anthropic` |

**Auth** (project key only — never upstream tokens):

```http
Authorization: Bearer sk-kano-proxy-...
```

Anthropic-style clients may also use `x-api-key: sk-kano-proxy-...`.

**Models** use OpenRouter-style ids:

```text
claude-code/claude-opus-5
codex/gpt-5.4
grok/grok-4.5
```

`GET …/models` lists only models available for the key owner’s currently usable accounts.

### Minimal OpenAI-style example

```bash
curl https://<your-domain>/openai/v1/chat/completions \
  -H "Authorization: Bearer sk-kano-proxy-..." \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-code/claude-opus-5",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

Point tools that accept a custom OpenAI base (CC Switch, Codex-style clients, OpenAI SDKs, etc.) at `/openai/v1` with the same key and model id.

---

## Providers

| Provider | Upstream surface | Pool |
|----------|------------------|------|
| **Claude Code** | Anthropic Messages (subscription OAuth) | Multi-account |
| **Codex** | ChatGPT Codex backend | Multi-account |
| **Grok** | xAI Chat Completions (SuperGrok OAuth) | Multi-account |

Pool behavior is the same once accounts are bound: priority, acquire, bench on 401/403/429, promote, remove.

---

## Built for coding agents

- **Streaming SSE** — no buffering of the full completion
- **Multi-turn tools** — `tools` / `tool_choice` and tool rounds
- **Vision** where the upstream supports it
- **JSON / schema** `response_format` when the provider allows
- **Reasoning effort** mapped per provider
- Anthropic **`cache_control` passthrough** (never rewritten)

---

## Admin UI

After Google sign-in you can:

- **Accounts** — bind / promote / remove pools; see usage windows (e.g. Claude 5h + week)
- **Models** — live catalog of `provider/model` ids for your bound accounts
- **Keys** — create and revoke API keys; copy OpenAI & Anthropic base URLs

Metadata is cache-first in the UI; proxy traffic itself is never served from a client cache of secrets.

---

## What this is not

- Not a shared Platform API-key reseller or aggregator
- Not embeddings / images / audio (chat + messages for coding agents first)
- Not a prompt audit store — content logging is intentionally out of scope

Details and rationale: [docs/product.md](./docs/product.md).

---

## Documentation

| Doc | Contents |
|-----|----------|
| [docs/index.md](./docs/index.md) | Full docs map |
| [docs/product.md](./docs/product.md) | Goals, tenants, model naming |
| [docs/api.md](./docs/api.md) | Public routes, errors, reasoning |
| [docs/auth.md](./docs/auth.md) | Google OIDC, API keys, account binding |
| [docs/deployment.md](./docs/deployment.md) | Local dev, secrets, Cloudflare deploy |
| [docs/providers.md](./docs/providers.md) | Pool / failover / adapters |
| [docs/admin-ui.md](./docs/admin-ui.md) | Web UI pages and UX |

**Local development & production deploy** live in [docs/deployment.md](./docs/deployment.md) — monorepo install, Wrangler, D1/KV, DNS, and smoke checks.

```bash
pnpm install
pnpm test
pnpm typecheck
```
