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

Displayed key prefixes (admin UI "Keys" list) store the **first 20 characters** of the plaintext key — the `sk-kano-proxy-` constant (14 chars) plus 6 distinguishing characters — so two keys are visually distinguishable in the list. Keys created before this change stored only the 14-character constant and continue to display identically to each other; this is a cosmetic limitation of already-issued rows, not a security issue — the full key is still hashed and compared in full on every request.

## OpenAI surface

### `POST /openai/v1/chat/completions`

Supported fields:

| Field | Behavior |
|-------|----------|
| `model` | Required. `provider/model` |
| `messages` | Required |
| `stream` | boolean; default false |
| `max_tokens` / `max_completion_tokens` | Forwarded as provider max output for `grok` and `claude-code`. **Never forwarded to `codex`** — the ChatGPT Codex backend rejects a top-level `max_output_tokens` with HTTP 400 `"Unsupported parameter"` for every verified OAuth model, so the field is accepted from the client but intentionally dropped before the upstream Responses call |
| `tools` | Forwarded via adapter |
| `tool_choice` | Forwarded via adapter. For `codex`, mapped to the Responses flattened shape: `"auto"` / `"none"` / `"required"` pass through as the same string; `{type:"function", function:{name}}` → `{type:"function", name}`; when omitted but `tools` is present, defaults to `"auto"`; when omitted and no `tools`, omitted upstream (the backend rejects `tool_choice` without `tools`) |
| `response_format` | json_object / json_schema when upstream supports |
| `reasoning_effort` | optional string (see below) |
| `stop` | string or string[]; forwarded to `grok`, and as Anthropic `stop_sequences` to `claude-code`. Dropped for `codex` — the Responses API has no equivalent |
| `prompt_cache_key` | optional string, official OpenAI Chat Completions field. Forwarded verbatim to `codex`'s Responses `prompt_cache_key` when non-empty (see below). **Ignored** for `grok` and `claude-code` |
| `temperature` | **Ignored** (stripped) for every built-in provider. A **custom openai-format** provider (see below) forwards it verbatim — it has no named handling here, so nothing strips it |
| image parts | Vision when upstream supports |

### Custom providers (`slug/upstream`, a user-defined endpoint)

`model` may also be `<slug>/<upstream_id>` for a user-defined custom endpoint (`docs/providers.md`). Resolution: split on the first `/`; if the prefix is a builtin `ProviderId` use the table above; otherwise look up `custom_providers` for the **authenticated user** by that slug — a miss (unknown slug, or a slug that belongs to a different user) is the same `400 invalid_model` as an unknown builtin. Behavior on `/openai/v1/chat/completions` depends on the provider's `format`:

| Custom `format` | Behavior |
|------------------|----------|
| `openai` | Near-passthrough to `{base_url}/chat/completions`: `model` rewritten to the bare upstream id, the rest of the client body forwarded **verbatim** — including `temperature`, `reasoning_effort` (unclamped, no provider ceiling), `response_format`, and any other field. This is a deliberate divergence from every built-in adapter. |
| `anthropic` | Converted via the same shared OpenAI↔Anthropic converters `claude-code` uses (`openaiToAnthropicMessages` / `anthropicToOpenAIResponse`), sent to `{base_url}/v1/messages`. `reasoning_effort` is **dropped** on this surface (no `thinking`/`output_config` synthesized) — use the native `/anthropic` surface for full `thinking` control. |

Neither custom format supports `prompt_cache_key`. See [providers.md](./providers.md) for the full endpoint-construction, auth-header, and SSRF-guard rules.

Streaming: SSE, OpenAI chunk shape, end-to-end without buffering entire completion.

### Codex request/response shape

- **System → `instructions`:** `role: "system"` messages are never sent as `input` items. Their text (in order, `"\n\n"`-joined for multiple system messages) becomes the top-level Responses `instructions` field instead; this applies whether the system message came from the OpenAI surface directly or from an Anthropic `system` block converted to an OpenAI system message on the `/anthropic` surface.
- **`store: false`:** every codex request sets `store: false` explicitly — the Responses API defaults to `store: true`, which this proxy does not want (no server-side conversation state is kept upstream).
- **`prompt_cache_key`:** forwarded as-is when the client sends a non-empty string. Account acquisition is deterministic (highest `priority`, then newest `created_at` — see [providers.md](./providers.md)), so the same upstream ChatGPT account normally serves every turn of a conversation, and a stable `prompt_cache_key` lets that account's upstream prompt cache actually hit.
- **`reasoning_content`:** when the upstream reasoning summary stream (`response.reasoning_summary_text.delta`) carries text, it is surfaced using the de-facto `reasoning_content` extension field (the DeepSeek/OpenRouter convention — there is no first-party OpenAI field for this): as `delta.reasoning_content` on streamed chunks, and as `message.reasoning_content` on the non-stream completion. On the `/anthropic` surface these chunks pass through the OpenAI→Anthropic converter harmlessly and are dropped (that converter only reads `content` / `tool_calls`); no Anthropic `thinking` block is synthesized from them.
- **Upstream failures mid-turn (`response.failed` / `error` events):** the ChatGPT backend can end a Responses SSE turn with a `response.failed` or `error` event instead of `response.completed` (rate limit, content policy, backend fault, etc.). This proxy never fabricates a `200` completion for that. Streaming: a single OpenAI-shaped error line (`data: {"error":{"message","type":"upstream_error"}}`) replaces the rest of the turn and the stream ends immediately — no `finish_reason` chunk, no `[DONE]`. Non-stream: the adapter returns `502` with `{"error":{"message","type":"upstream_error"}}` instead of a completion built from a partial/empty turn. On the `/anthropic` surface, the same failure converts to an Anthropic `event: error` (`{"type":"error","error":{"type":"api_error","message"}}`) and the stream ends there.

### Claude Code streaming usage

`claude-code` requests made through the OpenAI surface (`stream: true`) convert the upstream Anthropic Messages SSE into OpenAI chunks. Usage is not known until the upstream stream reports it, so — like the codex converter above — it rides on the **final** chunk rather than an early one: prompt tokens come from `message_start` (`usage.input_tokens` plus `cache_read_input_tokens` / `cache_creation_input_tokens`, summed the same way the non-stream response does), completion tokens from `message_delta.usage.output_tokens`, and the combined `{prompt_tokens, completion_tokens, total_tokens}` is attached to the last chunk (the one carrying `finish_reason`) whenever any of those counts were seen. An upstream Anthropic `event: error` mid-stream converts to an OpenAI-shaped error line and ends the stream — no trailing `finish_reason` chunk, no `[DONE]`.

### `GET /openai/v1/models`

Returns OpenAI-style `{ object: "list", data: [...] }` for providers the key owner has bound. Ids are `provider/upstream_id`. Claude Code and Grok come from live upstream `/models`. Codex returns empty (ChatGPT OAuth has no models list API — see [providers.md](./providers.md)). Empty for a provider when the user has no usable account for it. The user's custom providers are appended after the builtins — manual list, or live + cache + fallback for `models_mode=auto` (see [providers.md](./providers.md)).

## Anthropic surface

Same providers as the OpenAI surface. Model id is always `provider/upstream` (not bare).

### `POST /anthropic/v1/messages`

| `model` provider | Behavior |
|------------------|----------|
| `claude-code` | Anthropic Messages **passthrough** to Claude Code OAuth upstream: auth inject, fixed Claude Code system prepend if missing (identical string every time), `anthropic-beta` merged, **`cache_control` never rewritten** (block- or top-level). Upstream `model` field is the bare id after the prefix. When the (patched) request body contains an `output_config` key, `effort-2025-11-24` is added to the outgoing `anthropic-beta` header automatically (deduped against client-supplied betas) so upstream honors `output_config.effort`; clients that already send that beta are not double-added. |
| `grok` / `codex` | Convert Messages → internal Chat Completions shape → existing provider adapter → convert response/SSE back to Anthropic Messages. Streaming conversion includes **text and `tool_use`** (`input_json_delta` from OpenAI `tool_calls` argument chunks) so Claude Code / CC Switch can complete tool rounds on **grok and codex**. The codex streaming converter maps Responses events (`response.output_item.added` → tool_call header, `response.function_call_arguments.delta` → argument fragments, `response.output_item.done` as fallback when no deltas were seen) onto OpenAI `tool_calls` chunks, with `call_id` as the tool id so replayed history matches `function_call_output.call_id`. `stop_sequences` forwards as OpenAI `stop`; `system` blocks are joined with a blank line. Anthropic `cache_control` has no equivalent → **stripped on convert** (not forwarded, not reinvented as Grok sticky headers). Optional client headers `x-grok-conv-id` / `x-grok-session-id` / `x-grok-turn-idx` are forwarded on the Grok path when present; never synthesized. |
| custom, `format=anthropic` | Native **passthrough** to `{base_url}/v1/messages`, same shape as `claude-code` (auth inject, `model` rewritten to the bare upstream id, `cache_control`/`thinking` never touched) but with **none** of the Claude-Code-OAuth specifics: no system prepend, no auto-added effort beta, no fixed base betas — `anthropic-beta` is forwarded verbatim from the client (or omitted) and `anthropic-version` defaults to `2023-06-01` only when the client sends none. |
| custom, `format=openai` | Same conversion path as `grok`/`codex` (`cache_control` stripped, tool/vision conversion identical), landing on the custom-openai adapter's `chatCompletions()` instead of a builtin one. |

`model` **must** be `provider/upstream` (e.g. `claude-code/claude-opus-5`, `grok/grok-4.5`, or `<slug>/<upstream>` for a custom endpoint). Bare ids → `400` `invalid_model`. A slug that doesn't match one of the caller's own custom providers is the same `400 invalid_model` as an unknown builtin — a custom slug never resolves cross-user.

Converted streams follow the Anthropic content-block contract: blocks are strictly
sequential (one open at a time, dense ascending indices, never reopened). Because an
OpenAI chunk stream may interleave text with a call's `arguments` — and may alternate
fragments between several `tool_calls[].index` values — the converter streams the first
tool call live, buffers interleaved text into its own block, and emits any further calls
complete at the end of the turn. `usage` is taken from the upstream final chunk
(`stream_options.include_usage`) when the provider reports it.

**Server-side tools are dropped, not forwarded.** Anthropic tool definitions that carry a `type` other than `"custom"` and no `input_schema` (`web_search_*`, `bash_*`, `text_editor_*`, `computer_*`, `code_execution_*`, and future Anthropic-hosted tools) have no `grok`/`codex` equivalent — neither backend can execute them. The converter drops these tools entirely instead of forwarding a fake empty-schema function, which would otherwise invite an uncallable tool call. Client-defined tools (anything with `input_schema`, or no `type` / `type: "custom"`) still convert to OpenAI function tools; anything already OpenAI-shaped (`{function: {...}}`) passes through unchanged.

**Images inside `tool_result` are re-attached as a follow-up message.** `grok`/`codex` have no Anthropic-style multi-part `tool` message; when a `tool_result` block's content contains `image` blocks, the converted `role: "tool"` message keeps the text parts plus a short placeholder line, and is immediately followed by one `role: "user"` message carrying the image(s) as `image_url` parts (same base64 → data URI / URL conversion used for regular user content). A `tool_result` with no images converts as before (text only).

### `POST /anthropic/v1/messages/count_tokens`

Parses the body the same way as `/v1/messages` — same `model` requirement, same `400` envelopes for invalid JSON / invalid model id.

| `model` provider | Behavior |
|------------------|----------|
| `claude-code` | Passthrough to upstream `https://api.anthropic.com/v1/messages/count_tokens` with the same header construction as `/v1/messages` (OAuth bearer, `anthropic-version` default `2023-06-01`, beta header via `resolveBetaHeader`), the bare upstream model id, and the fixed Claude Code system prepend applied (idempotent — Claude Code clients already send that exact first system block). Reuses the same account pool / bench-on-401-403-429 failover loop as `/v1/messages`. Never streams — always a non-stream JSON response, returned as-is on success or upstream error. |
| `grok` / `codex` | `400`, no upstream call. There is no Chat Completions token-counting endpoint to convert to. Envelope: `{"type":"error","error":{"type":"invalid_request_error","message":"count_tokens is only supported for claude-code models"}}`. |
| custom, `format=anthropic` | Passthrough to `{base_url}/v1/messages/count_tokens`, same header construction (and same OAuth-specifics omissions) as that provider's `/v1/messages`. Reuses the same account pool / failover loop. |
| custom, `format=openai` | The exact same `400` rejection as `grok`/`codex` — no Chat Completions equivalent exists for a custom openai-format endpoint either. |

### `GET /anthropic/v1/models`

Same live catalog as `GET /openai/v1/models` for the key owner: all providers with usable accounts, including the user's custom providers appended after the builtins. Envelope is Anthropic-ish `{ data: [{ id, display_name, type: "model" }] }` with `id` = `provider/upstream`.

### Future

Same host keeps `/anthropic/*` for additional Anthropic routes if needed; do not break base URL clients already use.

## Model routing

1. Parse `provider` from `model` (`provider/rest` → provider, rest = upstream model id, split on the **first** `/` only — an upstream id may itself contain further `/`). Required on **both** surfaces.
2. If `provider` is a builtin `ProviderId`, use it directly. Otherwise look it up as a custom provider slug, scoped to the authenticated user (`custom_providers` table) — never resolves another user's slug.
3. Resolve user’s pool for that provider (or custom slug).
4. `acquire()` usable account; on 401/403/429 bench and try next.
5. No usable account → error (below).
6. No provider match at all (not a builtin id, not one of the caller's custom slugs) → `400 invalid_model`.

## `reasoning_effort`

Client field (OpenAI body; Anthropic may use same via extension or map from omitted):

`none` | `low` | `medium` | `high` | `xhigh` | `max` | omit

| Provider | Mapping | Ceiling |
|----------|---------|---------|
| grok | Top-level `reasoning_effort`; omit if unset; `none` only if model allows | `xhigh` |
| codex | Omit field if none/unset; else Responses `reasoning: { effort, summary: "auto" }` | `xhigh` |
| claude-code | Map to `output_config.effort`; off/`none` → thinking disabled + safe effort; **no public `thinking: adaptive` API** | `max` |
| custom, `format=openai` | Forwarded verbatim as top-level `reasoning_effort` (part of the near-passthrough body) | **none — never clamped** |
| custom, `format=anthropic` | **Dropped** on `/openai/v1` (no `thinking`/`output_config` synthesized); on native `/anthropic`, whatever `thinking` object the client sends goes through untouched — full control, no ceiling | n/a |

**Ceiling clamp — a valid effort above a *builtin* provider's ceiling is lowered to that provider's highest supported effort, never rejected.** Example: Claude Code running at `max` with sonnet/haiku remapped to `grok/...` or `codex/...` sends `xhigh` upstream instead of getting a `400`. Ceilings verified 2026-08-02: xAI documents `high` as grok-4.5's top and `xhigh` for grok-4.20-multi-agent, and live grok-4.5 accepts `xhigh` without error (reasoning tokens scale up), so grok's provider-wide ceiling is `xhigh`; codex Responses models (gpt-5.2-codex, gpt-5.1-codex-max) top out at `xhigh` — `max` exists only on non-codex GPT-5.6 models, which this adapter does not serve. **Custom providers are never clamped** — there is no known upstream ceiling for an arbitrary BYO endpoint, so a custom-openai provider forwards whatever the client sent (already validated against the ladder below) as-is.

Only unknown tokens (anything outside the ladder above) → `400` via `parseReasoningEffort`, for every provider including custom ones — the ladder validation happens before routing to any adapter. Unknown model ids: pass the (clamped, for builtins) effort through when possible.

**Reasoning is effort-only — `thinking.budget_tokens` is never mapped, for any provider.** A client-supplied Anthropic `thinking` object is dropped on the `grok`/`codex` conversion path (not forwarded — that converter only ever reads `reasoning_effort` / `output_config.effort`, never `thinking`) and passed through **untouched** on the native `claude-code` `/anthropic` passthrough path (the whole request body, `thinking` included, goes to Anthropic exactly as sent; this proxy does not rewrite it). The effort inputs this proxy actually reads, in priority order: (1) `reasoning_effort` — the OpenAI field, also accepted as a same-named optional field directly on an Anthropic request body; (2) if that is absent, `output_config.effort` on the Anthropic surface (`grok`/`codex` conversion only) as a fallback. Invalid `reasoning_effort` values still `400` via `parseReasoningEffort` regardless of which field supplied them.

## Prompt cache

| Path | Policy |
|------|--------|
| `/anthropic` → `claude-code` | **Strict passthrough** of all client `cache_control`. Do not reorder tools/system/messages or normalize away cache-relevant structure. Fixed system prepend is byte-stable when added. |
| `/openai/v1` → `claude-code` | **Do not add** top-level or block `cache_control` (these requests do not hit Anthropic prompt cache). |
| `/anthropic` → `grok` / `codex` | Strip `cache_control` on convert; no Anthropic-style breakpoints upstream. |
| `/openai/v1` or `/anthropic` → `grok` | Forward client `x-grok-conv-id` (and session/turn) when supplied; **never invent**. Prefix cache works without conv-id (see [providers.md](./providers.md)). |
| `/anthropic` → custom, `format=anthropic` | **Strict passthrough** of all client `cache_control`, same as `claude-code` (no system prepend to keep byte-stable, though — there isn't one). |
| `/openai/v1` → custom, `format=anthropic` | **Do not add** `cache_control` — same rule as `claude-code`. |
| `/anthropic` → custom, `format=openai` | Strip `cache_control` on convert, same as `grok`/`codex`. |

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

Auth failures (missing/invalid API key) are envelope-shaped per **surface**, matched on request path prefix rather than on provider: `/anthropic/*` gets the Anthropic shape `{"type":"error","error":{"type":"authentication_error","message":"Missing API key"|"Invalid API key"}}`; every other path (including `/openai/*`) keeps the OpenAI shape shown in the table above.

## Rate limits

No platform per-key quota. Upstream rate limits apply; pool benches on 401/403/429.
