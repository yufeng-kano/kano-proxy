# Product

## Goals

1. **OpenAI-compatible surface** (`/openai/v1`) for coding agents (CC Switch, Codex-style clients, OpenAI SDKs).
2. **Anthropic Messages surface** (`/anthropic`) for Claude Code and Anthropic-shaped clients.
3. **Every bound subscription provider** (`claude-code`, `codex`, `grok`) is available on **both** surfaces via format adapters — not “one API format per provider.”
4. **Per-user subscription account pools** (not shared Platform API keys as the product).
5. **User-defined custom upstream providers:** bring your own OpenAI- or Anthropic-compatible endpoint (base URL + API key) and it behaves like a built-in provider — a slug, model ids `slug/upstream`, both surfaces, pool/bench inherited (see [providers.md](./providers.md)).
6. **Model groups:** per-user bare-name aliases that expand to an ordered list of `provider/model` targets — one mechanism for both model mapping (client's hard-coded name → any target) and joining the same model across different accounts/prefixes, with cross-provider failover by target order (see [providers.md](./providers.md)).
7. **Admin web UI:** bind accounts, see 5h/Week (or weekly) usage %, manage API keys, manage custom endpoints and model groups.
8. **Multi-tenant isolation:** User A’s pool never serves User B.

## Non-goals (now)

- Official Platform API-key aggregation as primary product
- Usage-balanced / weighted routing across model-group targets (groups ship ordered-priority only; balancing needs stickiness first — see [providers.md](./providers.md))
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

Token **acquisition methods may differ**; once stored, pool semantics are the same: priority, acquire, bench on 401/402/403/429, promote, remove.

### Custom endpoints (user-defined)

Any user can also add their own OpenAI- or Anthropic-compatible endpoint (a self-hosted gateway, another provider's API, a proxy they run) by giving it a **slug**, a **base URL**, an **API key**, and a **format** (`openai` | `anthropic`). Once added it is a first-class provider on both surfaces with model ids `slug/upstream` and the same account-pool / bench / failover semantics as built-ins — see [providers.md](./providers.md) for the full contract, [database.md](./database.md) for the `custom_providers` table, and [auth.md](./auth.md) for the admin REST routes. Custom providers deliberately diverge from built-ins in two ways: an **openai**-format endpoint forwards the client's body near-verbatim (including `temperature` and `reasoning_effort`, with no ceiling clamp and no default — built-ins forward `temperature`/`top_p` too now, but `grok` and `claude-code` clamp/default it and `codex` still ignores it entirely, see [api.md](./api.md)), and neither format has a usage/billing surface to poll.

## Model naming

Same id on **both** surfaces (OpenAI and Anthropic):

```text
{provider}/{upstream_model_id}
```

Examples:

- `claude-code/claude-opus-5`
- `codex/gpt-5.4`
- `grok/grok-4.5`
- `<your-slug>/<upstream_model_id>` — a user-defined custom endpoint. Only the *first* `/` splits the id, so an upstream id that itself contains `/` (e.g. an OpenRouter-style `org/model`) still routes: `openrouter/anthropic/claude-3.7-sonnet` is slug `openrouter`, upstream id `anthropic/claude-3.7-sonnet`.

**One exception to the prefix rule:** a bare id (no `/`) that matches one of the caller's **model group** names expands to that group's first usable `provider/model` target before dispatch ([providers.md](./providers.md) § Model groups). Any other bare id is rejected. The missing slash is what keeps the two namespaces from ever colliding.

`GET /openai/v1/models` and `GET /anthropic/v1/models` list the same live catalog for the **authenticated user’s currently usable accounts** (ids always `provider/upstream`), plus the user's model groups under their bare names.

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
- `temperature` / `top_p`: forwarded to `grok` and `claude-code` (temperature clamped/defaulted per provider), **ignored** for `codex`; a **custom openai-format** provider forwards both verbatim (see [api.md](./api.md))

## Usage UI

| Provider | Windows |
|----------|---------|
| Claude | 5h %, Week %, optional scoped weekly rows |
| Codex | Dynamic windows from upstream (5h and/or Week; omit missing) |
| Grok | Weekly % from subscription billing surface (unofficial; stale on failure) |
| Custom endpoints | None — static API keys have no usage/billing surface to poll; the admin UI shows only a status dot (active/benched) |

## Success criteria

- Coding agent can set OpenAI base + key + `provider/model` and complete a multi-tool turn for any bound provider.
- Anthropic client can set Anthropic base + key + the **same** `provider/model` and complete Messages (Claude: tools + `cache_control` passthrough; Grok/Codex: converted).
- Admin can add multiple accounts, see usage bars, promote/remove, mint/revoke keys.
- Admin can add a custom OpenAI- or Anthropic-compatible endpoint (slug + base URL + API key) and immediately call it as `slug/model` on both surfaces, same as a built-in provider.
