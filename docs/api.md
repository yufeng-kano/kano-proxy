# Public LLM API

## Base URLs

Replace `<your-domain>` with the hostname you bind in Cloudflare (see [deployment.md](./deployment.md)). Admin UI and `GET /api/models` expose the live bases for the current host — nothing is hard-coded to a specific domain.

| Protocol | Base URL (client setting) |
|----------|---------------------------|
| OpenAI-compatible | `https://<your-domain>/openai/v1` |
| Anthropic Messages | `https://<your-domain>/anthropic` |

Local:

| Protocol | Base URL |
|----------|----------|
| OpenAI | `http://127.0.0.1:8787/openai/v1` |
| Anthropic | `http://127.0.0.1:8787/anthropic` |

Admin UI and management JSON live under `/` and `/api/*` (not under these bases).

## Authentication

All LLM routes require a **project-issued** API key:

```http
Authorization: Bearer sk-kano-proxy-...
```

Also accepted (Anthropic-style):

```http
x-api-key: sk-kano-proxy-...
```

Upstream OAuth tokens never appear in client requests.

## OpenAI surface

### `POST /openai/v1/chat/completions`

Supported fields:

| Field | Behavior |
|-------|----------|
| `model` | Required. `provider/model` |
| `messages` | Required |
| `stream` | boolean; default false |
| `max_tokens` / `max_completion_tokens` | Forwarded as provider max output |
| `tools` / `tool_choice` | Forwarded via adapter |
| `response_format` | json_object / json_schema when upstream supports |
| `reasoning_effort` | optional string (see below) |
| `temperature` | **Ignored** (stripped) |
| image parts | Vision when upstream supports |

Streaming: SSE, OpenAI chunk shape, end-to-end without buffering entire completion.

### `GET /openai/v1/models`

Returns OpenAI-style `{ object: "list", data: [...] }` for providers the key owner has bound. Ids are `provider/upstream_id`. Claude Code and Grok come from live upstream `/models`. Codex returns empty (ChatGPT OAuth has no models list API — see [providers.md](./providers.md)). Empty for a provider when the user has no usable account for it.

## Anthropic surface

### `POST /anthropic/v1/messages`

Anthropic Messages body **passthrough** to Claude Code upstream (subscription OAuth), with:

- Auth replaced by pool access token
- Optional fixed Claude Code system prepend (only if missing; identical string every time)
- `anthropic-beta` merged, not stripped
- **`cache_control` never rewritten** (block-level or top-level)

`model` may be:

- bare upstream id (`claude-opus-5`), or
- `claude-code/...` (provider prefix stripped for upstream)

Only **claude-code** pool serves this surface. Other providers → `400` with clear code.

### `GET /anthropic/v1/models`

Claude models available to this user (from pool / catalog), Anthropic-ish list shape or simplified `{ data: [{ id, display_name }] }` documented in OpenAPI comments in code.

### Future

Same host keeps `/anthropic/*` for additional Anthropic routes if needed; do not break base URL clients already use.

## Model routing

1. Parse `provider` from `model` (`provider/rest` → provider, rest = upstream model id).
2. Resolve user’s pool for that provider.
3. `acquire()` usable account; on 401/403/429 bench and try next.
4. No usable account → error (below).

## `reasoning_effort`

Client field (OpenAI body; Anthropic may use same via extension or map from omitted):

`none` | `low` | `medium` | `high` | `xhigh` | `max` | omit

| Provider | Mapping |
|----------|---------|
| grok | Top-level `reasoning_effort`; omit if unset; `none` only if model allows |
| codex | Omit field if none/unset; else Responses `reasoning: { effort, summary: "auto" }` |
| claude-code | Map to `output_config.effort`; off/`none` → thinking disabled + safe effort; **no public `thinking: adaptive` API** |

Invalid combo for known models → `400`. Unknown model ids: pass effort through when possible.

## Prompt cache (Anthropic)

| Path | Policy |
|------|--------|
| Native Anthropic | **Strict passthrough** of all `cache_control` |
| OpenAI → Claude | **Do not add** top-level or block `cache_control` |

Proxy must not reorder tools/system/messages or normalize away cache-relevant structure on Anthropic path.

## Errors

JSON error objects; OpenAI-ish or Anthropic-ish envelope depending on surface.

| Situation | HTTP | `code` (approx) |
|-----------|------|-----------------|
| Missing/invalid API key | 401 | `invalid_api_key` |
| Model string invalid | 400 | `invalid_model` |
| No account for provider | 400 | `no_upstream_account` |
| All accounts benched / unavailable | 503 | `upstream_unavailable` (+ `Retry-After` when known) |
| Upstream 4xx/5xx after retries | pass through status when possible | `upstream_error` |
| Reasoning rejected | 400 | `invalid_reasoning` |

## Rate limits

No platform per-key quota. Upstream rate limits apply; pool benches on 401/403/429.
