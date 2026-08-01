# Product

## Goals

1. **OpenAI-compatible surface** (`/openai/v1`) for coding agents (CC Switch, Codex-style clients, OpenAI SDKs).
2. **Anthropic Messages surface** (`/anthropic`) for Claude Code and Anthropic-shaped clients.
3. **Every bound subscription provider** (`claude-code`, `codex`, `grok`) is available on **both** surfaces via format adapters — not “one API format per provider.”
4. **Per-user subscription account pools** (not shared Platform API keys as the product).
5. **Admin web UI:** bind accounts, see 5h/Week (or weekly) usage %, manage API keys.
6. **Multi-tenant isolation:** User A’s pool never serves User B.

## Non-goals (now)

- Official Platform API-key aggregation as primary product
- Embeddings / images / audio
- Staging environment
- ToS / legal warning copy in UI
- Content logging or prompt audit storage
- Inventing Anthropic `cache_control` on OpenAI→Claude conversion
- Inventing Grok sticky headers (`x-grok-conv-id` etc.)

## Tenants

- Anyone can sign in with **Google OIDC**.
- Each user:
  - Binds zero or more upstream accounts per provider
  - Issues **multiple** client API keys (no expiry; revoke by delete)
  - Sees only their accounts, keys, and usage

## Providers (MVP = all)

| Provider id | Upstream | Login | Pool |
|-------------|----------|-------|------|
| `claude-code` | Anthropic Messages via Claude Code OAuth | Browser OAuth + paste `code#state` | Multi-account |
| `codex` | ChatGPT Codex backend | Browser OAuth (public redirect or paste) | Multi-account |
| `grok` | xAI Chat Completions via SuperGrok OAuth | Device code (and future ingest methods) | Multi-account |

Token **acquisition methods may differ**; once stored, pool semantics are the same: priority, acquire, bench on 401/403/429, promote, remove.

## Model naming

Same id on **both** surfaces (OpenAI and Anthropic):

```text
{provider}/{upstream_model_id}
```

Examples:

- `claude-code/claude-opus-5`
- `codex/gpt-5.4`
- `grok/grok-4.5`

`GET /openai/v1/models` and `GET /anthropic/v1/models` list the same live catalog for the **authenticated user’s currently usable accounts** (ids always `provider/upstream`). Bare upstream ids without a provider prefix are rejected.

## Client capabilities (required)

Must work for coding agents:

- Streaming SSE (no full-stream buffer)
- `messages` multi-turn including tool rounds
- `tools` / `tool_choice`
- Vision (`image_url` / Anthropic image blocks)
- `response_format` / JSON schema where upstream allows
- `max_tokens` / Anthropic `max_tokens`
- `reasoning_effort` (see [api.md](./api.md))
- `stop` / Anthropic `stop_sequences`
- `temperature` accepted but **stripped** (not forwarded)

## Usage UI

| Provider | Windows |
|----------|---------|
| Claude | 5h %, Week %, optional scoped weekly rows |
| Codex | Dynamic windows from upstream (5h and/or Week; omit missing) |
| Grok | Weekly % from subscription billing surface (unofficial; stale on failure) |

## Success criteria

- Coding agent can set OpenAI base + key + `provider/model` and complete a multi-tool turn for any bound provider.
- Anthropic client can set Anthropic base + key + the **same** `provider/model` and complete Messages (Claude: tools + `cache_control` passthrough; Grok/Codex: converted).
- Admin can add multiple accounts, see usage bars, promote/remove, mint/revoke keys.
