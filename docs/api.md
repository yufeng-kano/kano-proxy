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
| `temperature` | `grok`: forwarded when sent; defaults to `1` when the client omits it (see "Grok sampling defaults" below). `claude-code`: forwarded when sent, clamped to Anthropic's `0–1` range (OpenAI's client-facing range is `0–2`). `codex`: **ignored** — no sampling field is ever added to the Responses body. A **custom openai-format** provider forwards it verbatim, unclamped, no default |
| `top_p` | `grok` and `claude-code`: forwarded only when the client sends it — no invented default. `codex`: **ignored**, same as `temperature`. Custom openai-format: forwarded verbatim |
| image parts | Vision when upstream supports |

**Grok sampling defaults.** On `/openai/v1` (Chat Completions → `api.x.ai`), xAI documents no default `temperature`; a separate xAI surface (`/v1/responses`) was observed 2026-08-02 defaulting to `temperature 0.7` / `top_p 0.95`. Since Anthropic and OpenAI both document `1` as their own default, the OpenAI-surface `grok/*` path pins `temperature: 1` explicitly whenever the client sends none. The `/anthropic` → `grok` path uses Responses (below) and applies the same pin when the client omits temperature.

**Grok reasoning — two upstream surfaces.**

| Client surface | Upstream | Reasoning shape |
|----------------|----------|-----------------|
| `/openai/v1` → `grok` | `api.x.ai` Chat Completions | `include_reasoning: true` → de-facto `reasoning_content` (DeepSeek/OpenRouter). Near-passthrough. |
| `/anthropic` → `grok` | `cli-chat-proxy.grok.com` Responses | `include: ["reasoning.encrypted_content"]` → Anthropic `thinking` **with** `signature` (= opaque `encrypted_content`). |

OpenAI-surface details: without `include_reasoning`, Chat Completions returns only `usage.completion_tokens_details.reasoning_tokens` as a count; with it, non-stream carries `message.reasoning_content` and streaming carries `delta.reasoning_content` before content/tool deltas. `reasoning_tokens`, when reported, is folded into logged/returned `completion_tokens` (and Anthropic `output_tokens` on converted paths) — see [logging.md](./logging.md).

Anthropic-surface details (Claude Code / CC Switch): Responses `reasoning.encrypted_content` maps to `thinking.signature` (stream: `signature_delta`). Summary/text when present maps to `thinking` / `thinking_delta`. On a later turn, a validated assistant `thinking.signature` is replayed as a Responses `input` item `{type:"reasoning", encrypted_content}` — this proxy never invents Claude-native signatures. A session-scoped KV replay cache (keyed by API key id + **upstream model** + client `x-grok-conv-id` / `x-grok-session-id`, never cross-user/model) can re-inject the last turn's encrypted reasoning when the client omits the signature but continues the same session — see [providers.md](./providers.md). Upstream EOF without `response.completed` surfaces as Anthropic `event: error`, not a fabricated successful turn.

**Thinking / effort on `/anthropic` → `grok`.** Honored (not ignored):

| Client signal | Upstream effect |
|---------------|-----------------|
| `thinking.type = "disabled"` | **Authoritative.** No `include`, no `reasoning` object (even if `output_config.effort` / `reasoning_effort` is present); response emits **no** thinking blocks |
| `thinking.type = "adaptive"` / `"enabled"` / `"auto"` | `include: ["reasoning.encrypted_content"]`; effort from `output_config.effort` or `reasoning_effort` (ceiling-clamped); default `medium` when neither is set |
| no `thinking` object, but effort present | Same as adaptive/enabled |
| no `thinking`, no effort | Include encrypted reasoning (agent-friendly default); no effort field |

Foreign / non-Grok `thinking.signature` values (Claude-native, GPT `gAAAA…`, provider-prefixed envelopes, low-entropy blobs) are **dropped** on convert — never forwarded as Responses `encrypted_content`. Only transport-valid Grok ciphertext is replayed.

`thinking.budget_tokens` is still **not** mapped to an effort ladder (effort-only). The OpenAI→Anthropic convert path used by `codex` / custom-openai is unchanged (unsigned `reasoning_content` → thinking; no signature).

> **xAI Chat Completions egress gate (verified 2026-08-02).** On the `/openai/v1` Chat Completions path, xAI strips plaintext `reasoning_content` from Cloudflare egress while still reporting `reasoning_tokens`. The `/anthropic` → Responses path uses opaque `encrypted_content` instead, which is what Claude Code needs for multi-turn thinking continuity and is not subject to that plaintext strip.

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
- **`reasoning_content`:** when the upstream reasoning summary stream (`response.reasoning_summary_text.delta`) carries text, it is surfaced using the de-facto `reasoning_content` extension field (the DeepSeek/OpenRouter convention — there is no first-party OpenAI field for this): as `delta.reasoning_content` on streamed chunks, and as `message.reasoning_content` on the non-stream completion. On the `/anthropic` surface this converts into a leading, **unsigned** Anthropic `thinking` content block — the same mechanism grok's `reasoning_content` uses (see the Grok reasoning note above and "Thinking blocks on the conversion path" below).
- **Upstream failures mid-turn (`response.failed` / `error` events):** the ChatGPT backend can end a Responses SSE turn with a `response.failed` or `error` event instead of `response.completed` (rate limit, content policy, backend fault, etc.). This proxy never fabricates a `200` completion for that. Streaming: a single OpenAI-shaped error line (`data: {"error":{"message","type":"upstream_error"}}`) replaces the rest of the turn and the stream ends immediately — no `finish_reason` chunk, no `[DONE]`. Non-stream: the adapter returns `502` with `{"error":{"message","type":"upstream_error"}}` instead of a completion built from a partial/empty turn. On the `/anthropic` surface, the same failure converts to an Anthropic `event: error` (`{"type":"error","error":{"type":"api_error","message"}}`) and the stream ends there.

### Claude Code streaming usage

`claude-code` requests made through the OpenAI surface (`stream: true`) convert the upstream Anthropic Messages SSE into OpenAI chunks. Usage is not known until the upstream stream reports it, so — like the codex converter above — it rides on the **final** chunk rather than an early one: prompt tokens come from `message_start` (`usage.input_tokens` plus `cache_read_input_tokens` / `cache_creation_input_tokens`, summed the same way the non-stream response does), completion tokens from `message_delta.usage.output_tokens`, and the combined `{prompt_tokens, completion_tokens, total_tokens}` is attached to the last chunk (the one carrying `finish_reason`) whenever any of those counts were seen. An upstream Anthropic `event: error` mid-stream converts to an OpenAI-shaped error line and ends the stream — no trailing `finish_reason` chunk, no `[DONE]`.

### Usage cache details on converted responses

Wherever this proxy **builds** an OpenAI-shaped `usage` object (claude-code / custom-anthropic conversions, codex Responses conversions — stream final chunk and non-stream alike), it attaches the upstream cache numbers instead of discarding them: `prompt_tokens_details.cached_tokens` (official OpenAI field; from Anthropic `cache_read_input_tokens` or Responses `input_tokens_details.cached_tokens`) and, for Anthropic-shaped upstreams only, a `cache_creation_input_tokens` extension field alongside it. `prompt_tokens` remains the cache-inclusive total.

Conversely, when building an Anthropic-shaped `usage` from an OpenAI upstream that reported `prompt_tokens_details.cached_tokens` (`/anthropic` → grok / codex / custom `format=openai`), the converted usage reports proper Anthropic semantics: `input_tokens` excludes the cached share, which appears as `cache_read_input_tokens`.

Pure passthrough paths (`grok` and custom `format=openai` on `/openai/v1`; native `/anthropic` passthroughs) never rewrite upstream usage. These numbers also feed `request_logs` for the admin dashboard (see [logging.md](./logging.md)).

### `GET /openai/v1/models`

Returns OpenAI-style `{ object: "list", data: [...] }` for providers the key owner has bound. Ids are `provider/upstream_id`. Claude Code and Grok come from live upstream `/models`. Codex returns empty (ChatGPT OAuth has no models list API — see [providers.md](./providers.md)). Empty for a provider when the user has no usable account for it. The user's custom providers are appended after the builtins — manual list, or live + cache + fallback for `models_mode=auto` (see [providers.md](./providers.md)).

## Anthropic surface

Same providers as the OpenAI surface. Model id is always `provider/upstream` (not bare).

### `POST /anthropic/v1/messages`

| `model` provider | Behavior |
|------------------|----------|
| `claude-code` | Anthropic Messages **passthrough** to Claude Code OAuth upstream: auth inject, fixed Claude Code system prepend if missing (identical string every time), `anthropic-beta` merged, **`cache_control` never rewritten** (block- or top-level). Upstream `model` field is the bare id after the prefix. When the (patched) request body contains an `output_config` key, `effort-2025-11-24` is added to the outgoing `anthropic-beta` header automatically (deduped against client-supplied betas) so upstream honors `output_config.effort`; clients that already send that beta are not double-added. |
| `grok` | Anthropic Messages → xAI **Responses** (`cli-chat-proxy.grok.com/v1/responses`) → Anthropic Messages. Streaming conversion includes **text, signed `thinking`, and `tool_use`**. `thinking.signature` ↔ Responses `reasoning.encrypted_content`; stream emits `signature_delta`. Tools / vision / `output_format.json_schema` convert; **`stop_sequences` is dropped** (Responses has no Chat Completions `stop` equivalent — same as codex). Anthropic `cache_control` stripped. Optional client headers `x-grok-conv-id` / `x-grok-session-id` / `x-grok-turn-idx` forwarded when present; never synthesized. Thinking/effort rules and the reasoning replay cache: see "Grok reasoning" above and [providers.md](./providers.md). The degenerate tool-call loop guard still applies (this is a conversion path, not Claude-native passthrough). |
| `codex` | Convert Messages → internal Chat Completions shape → existing provider adapter → convert response/SSE back to Anthropic Messages. Streaming conversion includes **text and `tool_use`** (`input_json_delta` from OpenAI `tool_calls` argument chunks) so Claude Code / CC Switch can complete tool rounds. The codex streaming converter maps Responses events (`response.output_item.added` → tool_call header, `response.function_call_arguments.delta` → argument fragments, `response.output_item.done` as fallback when no deltas were seen) onto OpenAI `tool_calls` chunks, with `call_id` as the tool id so replayed history matches `function_call_output.call_id`. `stop_sequences` forwards as OpenAI `stop`; `system` blocks are joined with a blank line. Anthropic `cache_control` has no equivalent → **stripped on convert**. |
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

**Thinking blocks on the conversion path.**

- **`/anthropic` → `grok` (Responses):** leading `thinking` block **with** `signature` when upstream returned `encrypted_content` (stream: `thinking_delta` + `signature_delta` before `content_block_stop`). No signature is invented — absent encrypted content ⇒ no signature field. Round-trip: assistant `thinking.signature` → Responses `reasoning` input `encrypted_content`; thinking plaintext is not required for replay. `redacted_thinking` dropped. When `thinking.type=disabled`, no thinking blocks are emitted even if upstream sent reasoning.
- **`/anthropic` → `codex` / custom-openai (Chat Completions convert):** unsigned thinking from de-facto `reasoning_content` (`{type:"thinking", thinking:"..."}` — no `signature`). Streamed live when reasoning precedes text/tools; late fragments are buffered and flushed as one complete block at end of turn. Round-trip: thinking text → `reasoning_content` on the OpenAI-shaped assistant message; codex's Responses builder ignores that field; custom-openai forwards it verbatim.

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
| grok | `/openai/v1`: top-level Chat Completions `reasoning_effort`; omit if unset. `/anthropic`: Responses `reasoning.effort` from `output_config.effort` / `reasoning_effort` / thinking mode (see "Grok reasoning"); **omit `reasoning` entirely** when `thinking.type=disabled` | `xhigh` |
| codex | Omit field if none/unset; else Responses `reasoning: { effort, summary: "auto" }` | `xhigh` |
| claude-code | Map to `output_config.effort`; off/`none` → thinking disabled + safe effort; **no public `thinking: adaptive` API** | `max` |
| custom, `format=openai` | Forwarded verbatim as top-level `reasoning_effort` (part of the near-passthrough body) | **none — never clamped** |
| custom, `format=anthropic` | **Dropped** on `/openai/v1` (no `thinking`/`output_config` synthesized); on native `/anthropic`, whatever `thinking` object the client sends goes through untouched — full control, no ceiling | n/a |

**Ceiling clamp — a valid effort above a *builtin* provider's ceiling is lowered to that provider's highest supported effort, never rejected.** Example: Claude Code running at `max` with sonnet/haiku remapped to `grok/...` or `codex/...` sends `xhigh` upstream instead of getting a `400`. Ceilings verified 2026-08-02: xAI documents `high` as grok-4.5's top and `xhigh` for grok-4.20-multi-agent, and live grok-4.5 accepts `xhigh` without error (reasoning tokens scale up), so grok's provider-wide ceiling is `xhigh`; codex Responses models (gpt-5.2-codex, gpt-5.1-codex-max) top out at `xhigh` — `max` exists only on non-codex GPT-5.6 models, which this adapter does not serve. **Custom providers are never clamped** — there is no known upstream ceiling for an arbitrary BYO endpoint, so a custom-openai provider forwards whatever the client sent (already validated against the ladder below) as-is.

Only unknown tokens (anything outside the ladder above) → `400` via `parseReasoningEffort`, for every provider including custom ones — the ladder validation happens before routing to any adapter. Unknown model ids: pass the (clamped, for builtins) effort through when possible.

**Reasoning is effort-only — `thinking.budget_tokens` is never mapped, for any provider.** Provider handling of the Anthropic `thinking` object:

| Path | `thinking` object |
|------|-------------------|
| `/anthropic` → `claude-code` (and custom `format=anthropic`) | Passed through **untouched** |
| `/anthropic` → `grok` | **Honored** for enable/disable + adaptive/enabled mode; effort from `output_config.effort` / `reasoning_effort` (see "Grok reasoning"). `budget_tokens` ignored |
| `/anthropic` → `codex` / custom-openai | Dropped — that Chat Completions convert only reads `reasoning_effort` / `output_config.effort` |

Effort inputs, in priority order where applicable: (1) `reasoning_effort` (OpenAI field, also accepted on an Anthropic body); (2) `output_config.effort` on the Anthropic surface. Invalid values still `400` via `parseReasoningEffort`.

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

## Streaming

Every streaming response, both surfaces, every provider, is relayed byte-for-byte from upstream — this proxy never buffers a whole completion before forwarding it — through a shared keepalive wrapper (`proxy/sse.ts`):

- **Keepalive comments** (`: keepalive\n\n`, an SSE comment line clients ignore) fire every 30s of silence — Cloudflare's own idle-connection mitigation. This now re-arms after **every** real upstream chunk, so any silence gap anywhere in the stream gets keepalives, not just the gap before the first token (a long tool-use or reasoning turn that goes quiet mid-stream used to stop getting them after the first byte).
- **120s upstream idle timeout.** If no real upstream chunk (keepalive comments do not count) arrives for 120s, the proxy gives up on that upstream connection: it emits one final stall frame, then ends the stream cleanly. The response's HTTP status/headers were already sent when streaming started (always `200`), so this is a mid-stream error event, not an HTTP-level error — clients treat it the same as any other mid-turn `event: error` / OpenAI error chunk from the upstream itself (see codex's "Upstream failures mid-turn" above). Frame shape is surface-specific:
  - Anthropic surface: `event: error\ndata: {"type":"error","error":{"type":"overloaded_error","message":"upstream stalled: no data received for 120s"}}\n\n`
  - OpenAI surface: `data: {"error":{"message":"upstream stalled: no data received for 120s","type":"api_error","code":"upstream_stall"}}\n\n`

  Logged as `error_code: "upstream_stall"` unconditionally on an idle-timeout close — see [logging.md](./logging.md) for the full close-reason → `error_code` mapping.

## Degenerate tool-call loop guard

A converted agent (`grok`, `codex`, or a custom `format=openai` provider; never native `claude-code`/custom `format=anthropic` passthrough) can get stuck calling the identical tool with identical arguments indefinitely, burning usage with no upstream error to signal it. Before dispatching a conversion-path request — on **both** surfaces — this proxy walks the trailing message history for a run of identical tool-call/tool-result rounds. Exempt adapters are native Anthropic passthrough only (`claude-code` / custom `format=anthropic`). `grok` exposes `messages()` for the `/anthropic` → Responses path but is **not** exempt (still a conversion path):

- **Anthropic shape** (`/anthropic/v1/messages` body, before conversion): a *unit* is an assistant message whose content contains exactly one `tool_use` block (accompanying text blocks are ignored for identity), immediately followed by a user message carrying a `tool_result` for it. Identity = tool name + `JSON.stringify(input)`; `tool_result` contents are irrelevant to identity.
- **OpenAI shape** (`/openai/v1/chat/completions` body): a unit is an assistant message with exactly one `tool_calls` entry, immediately followed by a `role: "tool"` result. Identity = `function.name` + the raw `function.arguments` string.
- A trailing run of **8 or more** identical, consecutive units trips the guard (7 does not). An assistant message with two-or-more tool calls in the same turn, a differing identity partway back, or a plain trailing turn (no tool round at the very tail) all stop the run from extending further.

On trip: `400`, no upstream call, logged with `error_code: "loop_detected"` (authenticated only — see [logging.md](./logging.md)):

- Anthropic: `{"type":"error","error":{"type":"invalid_request_error","message":"degenerate tool-call loop detected: <name> repeated <n> times with identical input; aborting so the client can recover"}}`
- OpenAI: `{"error":{"message":"degenerate tool-call loop detected: <name> repeated <n> times with identical input; aborting so the client can recover","type":"invalid_request_error","code":"loop_detected"}}`

`claude-code` and custom `format=anthropic` are exempt — both are native Anthropic passthrough, never converted, so this proxy never reshapes their tool-call history to begin with.

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
| Degenerate tool-call loop (conversion path only — see above) | 400 | `loop_detected` |

Auth failures (missing/invalid API key) are envelope-shaped per **surface**, matched on request path prefix rather than on provider: `/anthropic/*` gets the Anthropic shape `{"type":"error","error":{"type":"authentication_error","message":"Missing API key"|"Invalid API key"}}`; every other path (including `/openai/*`) keeps the OpenAI shape shown in the table above.

Authenticated pre-dispatch failures (invalid model, no upstream account, loop-guard trip) are all logged as one `request_logs` row via `waitUntil`, same as a real dispatch; unauthenticated 401s are never logged — see [logging.md](./logging.md).

## Rate limits

No platform per-key quota. Upstream rate limits apply; pool benches on 401/403/429.

## Changelog (admin)

### `GET /api/changelog`

Session-auth JSON for the admin UI: the running Worker version, the newest published release, an update flag, and the sanitized release list — sourced from this repo's GitHub Releases, cached in KV. `?refresh=true` bypasses the freshness window. Full contract (response shape, caching, stale-serve, sanitization) in [changelog.md](./changelog.md).
