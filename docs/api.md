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

Same providers as the OpenAI surface. Model id is always `provider/upstream` (not bare).

### `POST /anthropic/v1/messages`

| `model` provider | Behavior |
|------------------|----------|
| `claude-code` | Anthropic Messages **passthrough** to Claude Code OAuth upstream: auth inject, fixed Claude Code system prepend if missing (identical string every time), `anthropic-beta` merged, **`cache_control` never rewritten** (block- or top-level). Upstream `model` field is the bare id after the prefix. |
| `grok` / `codex` | Convert Messages → internal Chat Completions shape → existing provider adapter → convert response/SSE back to Anthropic Messages. Anthropic `cache_control` has no equivalent → **stripped on convert** (not forwarded, not reinvented as Grok sticky headers). Optional client headers `x-grok-conv-id` / `x-grok-session-id` / `x-grok-turn-idx` are forwarded on the Grok path when present; never synthesized. |

`model` **must** be `provider/upstream` (e.g. `claude-code/claude-opus-5`, `grok/grok-4.5`). Bare ids → `400` `invalid_model`.

### `GET /anthropic/v1/models`

Same live catalog as `GET /openai/v1/models` for the key owner: all providers with usable accounts. Envelope is Anthropic-ish `{ data: [{ id, display_name, type: "model" }] }` with `id` = `provider/upstream`.

### Future

Same host keeps `/anthropic/*` for additional Anthropic routes if needed; do not break base URL clients already use.

## Model routing

1. Parse `provider` from `model` (`provider/rest` → provider, rest = upstream model id). Required on **both** surfaces.
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

## Prompt cache

| Path | Policy |
|------|--------|
| `/anthropic` → `claude-code` | **Strict passthrough** of all client `cache_control`. Do not reorder tools/system/messages or normalize away cache-relevant structure. Fixed system prepend is byte-stable when added. |
| `/openai/v1` → `claude-code` | **Do not add** top-level or block `cache_control` (these requests do not hit Anthropic prompt cache). |
| `/anthropic` → `grok` / `codex` | Strip `cache_control` on convert; no Anthropic-style breakpoints upstream. |
| `/openai/v1` or `/anthropic` → `grok` | Forward client `x-grok-conv-id` (and session/turn) when supplied; **never invent**. Prefix cache works without conv-id (see [providers.md](./providers.md)). |

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
