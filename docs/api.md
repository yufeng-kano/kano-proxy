# Public LLM API

## Base URLs

Replace `<your-domain>` with the hostname you bind in Cloudflare (see [deployment.md](./deployment.md)). Admin UI and `GET /api/models` expose the live bases for the current host — nothing is hard-coded to a specific domain.

| Protocol | Base URL (client setting) |
|----------|---------------------------|
| OpenAI-compatible | `https://<your-domain>/openai/v1` |
| Anthropic Messages | `https://<your-domain>/anthropic` |
| OpenAI-compatible, one model group | `https://<your-domain>/g/<group-slug>/openai/v1` |
| Anthropic Messages, one model group | `https://<your-domain>/g/<group-slug>/anthropic` |

Local:

| Protocol | Base URL |
|----------|----------|
| OpenAI | `http://127.0.0.1:8787/openai/v1` |
| Anthropic | `http://127.0.0.1:8787/anthropic` |
| Group endpoints | `http://127.0.0.1:8787/g/<group-slug>/…` (same two shapes) |

The **shared** bases accept only `provider/model` ids. Each **model group** ([providers.md](./providers.md) § Model groups) is its own virtual endpoint under `/g/<slug>/` accepting only the model names that group defines — see "Group endpoints" below. Admin UI and management JSON live under `/` and `/api/*` (not under these bases).

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
| `model` | Required. `provider/model` on this shared base; on a group endpoint, one of that group's model names (see "Model routing" and "Group endpoints") |
| `messages` | Required |
| `stream` | boolean; default false |
| `max_tokens` / `max_completion_tokens` | Forwarded as provider max output for `grok`, `claude-code`, and `antigravity` (as Gemini `generationConfig.maxOutputTokens` — dropped for a Gemini model that also carries tools or a response schema, per [providers.md](./providers.md) § Antigravity). **Never forwarded to `codex`** — the ChatGPT Codex backend rejects a top-level `max_output_tokens` with HTTP 400 `"Unsupported parameter"` for every verified OAuth model, so the field is accepted from the client but intentionally dropped before the upstream Responses call |
| `tools` | Forwarded via adapter |
| `tool_choice` | Forwarded via adapter. For `codex`, mapped to the Responses flattened shape: `"auto"` / `"none"` / `"required"` pass through as the same string; `{type:"function", function:{name}}` → `{type:"function", name}`; when omitted but `tools` is present, defaults to `"auto"`; when omitted and no `tools`, omitted upstream (the backend rejects `tool_choice` without `tools`) |
| `response_format` | `json_schema` → Anthropic `output_config.format` for `claude-code` / custom `format=anthropic` (the current field; Anthropic rejects the retired top-level `output_format` with `400 This field is deprecated`), Responses `text.format` for `codex`, verbatim for custom `format=openai`; `json_object` is dropped on the Anthropic conversions |
| `reasoning_effort` | optional string (see below) |
| `stop` | string or string[]; forwarded to `grok`, as Anthropic `stop_sequences` to `claude-code`, and as Gemini `generationConfig.stopSequences` to `antigravity`. Dropped for `codex` — the Responses API has no equivalent |
| `prompt_cache_key` | optional string, official OpenAI Chat Completions field. Forwarded to `codex`'s Responses `prompt_cache_key` when non-empty, **shortened to OpenAI's 64-char limit** first (see below); for `codex` it also feeds the `session_id` header, which is fitted to the **same** 64-char limit (the backend validates that header under the `prompt_cache_key` name), and the reasoning-replay session fallback, which keeps the full key because it never leaves the Worker. **Ignored** for `grok` and `claude-code`. The `/anthropic` wire format has no such field — the conversion derives it from Anthropic `metadata.user_id` instead (see "Codex Responses details") |
| `temperature` | `grok`: forwarded when sent; defaults to `1` when the client omits it (see "Grok sampling defaults" below). `claude-code`: forwarded when sent, clamped to Anthropic's `0–1` range (OpenAI's client-facing range is `0–2`), **but dropped entirely when thinking is on** (see "Sampling under thinking" below). `antigravity`: forwarded when sent as Gemini `generationConfig.temperature`, no default. `codex`: **ignored** — no sampling field is ever added to the Responses body. A **custom openai-format** provider forwards it verbatim, unclamped, no default |
| `top_p` | `grok`, `claude-code` and `antigravity`: forwarded only when the client sends it — no invented default (`antigravity` as Gemini `topP`). `claude-code` additionally drops it under thinking unless it falls in `[0.95, 1]` (below). `codex`: **ignored**, same as `temperature`. Custom openai-format: forwarded verbatim |
| image parts | Vision when upstream supports |
| `input_audio` parts | Accepted on this surface only, and only for providers whose upstream wire can carry audio — see "Audio input" below |

**Sampling under thinking (`claude-code`).** Anthropic rejects `temperature` and `top_k` while thinking is active, and accepts `top_p` only within `[0.95, 1]`; on the newest models (Opus 5 / Sonnet 5 / Fable 5 / Opus 4.8 / 4.7 and Mythos) any non-default sampling value is a `400` regardless of thinking. The `/openai/v1` → `claude-code` conversion turns a client `reasoning_effort` into `output_config: {effort}` (see "Reasoning" below), so a request combining an effort with a plain `temperature` would otherwise `400` on a pairing the client never asked for. The converter therefore **drops** `temperature` — and a `top_p` outside `[0.95, 1]` — whenever thinking is on, i.e. whenever `thinking` or `output_config` is present and `thinking.type` is not `disabled`. Values are dropped, not silently retuned into range. Requests with no thinking config keep whatever sampling the client sent (temperature still clamped to `0–1`). The native `/anthropic` passthrough is unaffected: there the client owns the body and its own `thinking`/sampling combination.

**Grok sampling defaults.** On `/openai/v1` (Chat Completions → `api.x.ai`), xAI documents no default `temperature`; a separate xAI surface (`/v1/responses`) was observed 2026-08-02 defaulting to `temperature 0.7` / `top_p 0.95`. Since Anthropic and OpenAI both document `1` as their own default, the OpenAI-surface `grok/*` path pins `temperature: 1` explicitly whenever the client sends none. The `/anthropic` → `grok` path uses Responses (below) and applies the same pin when the client omits temperature.

**Grok reasoning — two upstream surfaces.**

| Client surface | Upstream | Reasoning shape |
|----------------|----------|-----------------|
| `/openai/v1` → `grok` | `api.x.ai` Chat Completions | `include_reasoning: true` → de-facto `reasoning_content` (DeepSeek/OpenRouter). Near-passthrough. |
| `/anthropic` → `grok` | `cli-chat-proxy.grok.com` Responses | `include: ["reasoning.encrypted_content"]` → Anthropic `thinking` **with** `signature` (= opaque `encrypted_content`). |

OpenAI-surface details: without `include_reasoning`, Chat Completions returns only `usage.completion_tokens_details.reasoning_tokens` as a count; with it, non-stream carries `message.reasoning_content` and streaming carries `delta.reasoning_content` before content/tool deltas. `completion_tokens` is already inclusive of reasoning tokens (and maps directly to Anthropic `output_tokens` on converted paths) — see [logging.md](./logging.md).

Anthropic-surface details (Claude Code / CC Switch): Responses `reasoning.encrypted_content` maps to `thinking.signature` (stream: `signature_delta`). Summary/text when present maps to `thinking` / `thinking_delta`. On a later turn, a validated assistant `thinking.signature` is replayed as a Responses `input` item `{type:"reasoning", encrypted_content}` — this proxy never invents Claude-native signatures. A session-scoped KV replay cache (keyed by API key id + **upstream model** + client `x-grok-conv-id` / `x-grok-session-id`, never cross-user/model) can re-inject the last turn's encrypted reasoning when the client omits the signature but continues the same session — see [providers.md](./providers.md). Upstream EOF without `response.completed` surfaces as Anthropic `event: error`, not a fabricated successful turn.

**Opaque decode recovery.** If cli-chat-proxy returns HTTP 400 with `Could not decode the compaction blob` or `Could not decrypt the provided encrypted_content`, the adapter clears the session replay cache, strips `reasoning.encrypted_content` (and any `compaction` input items), and retries once on the same account. If that still fails and the client sent sticky `x-grok-*` ids, it retries once more without those headers / `prompt_cache_key` (never when `previous_response_id` is set). Unrecovered failures return the original upstream 400.

**Thinking + tool_use ordering.** Upstream may emit `function_call` / `output_text` before `reasoning.output_item.done`. The SSE converter holds those events until the reasoning item finishes so `signature_delta` carries the final `encrypted_content` and still precedes `tool_use`. Closing thinking early (or emitting the preliminary blob from `output_item.added`) corrupts the assistant message that Claude Code fork/subagent workers inherit, which then fails the next turn with the compaction-blob 400 above.

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

### Audio input

`/openai/v1/chat/completions` accepts the OpenAI / OpenRouter audio content part inside a user message:

```json
{"type": "input_audio", "input_audio": {"data": "<base64>", "format": "wav"}}
```

`data` is raw base64 or a `data:<mime>;base64,…` URL — when it is a data URL its own mime wins over `format`. Nothing else about the request changes.

**Per provider.** "Carries audio" below means the body this proxy builds for that provider has somewhere to put the part. It is not a promise that the model accepts it — that answer comes from upstream:

| Provider | Audio part |
|----------|------------|
| `antigravity` | Converted to a Gemini `inlineData` part (`mimeType` + base64). Gemini models answer from the audio and bill it as prompt tokens (~32 tok/s). A **Claude** model served behind Antigravity has no audio input, so that combination is an upstream rejection, not a proxy one |
| `grok` | Forwarded verbatim inside `messages` to `api.x.ai` — xAI decides |
| custom, `format=openai` | Forwarded verbatim in the near-passthrough body — the endpoint decides (this is how OpenRouter's audio-capable models are reached) |
| `codex` | **`400 unsupported_modality`.** The Responses input this proxy builds has no audio content type, and no OAuth Codex model accepts audio |
| `claude-code`, custom `format=anthropic` | **`400 unsupported_modality`.** Anthropic Messages defines no audio content block, so there is nothing to convert to |

The rejection is decided from the **highest-priority resolved target only** — the same rule the loop guard uses — before any upstream call, and is logged with `error_code: "unsupported_modality"` and `x-should-retry: false`. Audio is never silently dropped: it either reaches the upstream or the request fails loudly.

**`format` → mime, on conversion targets only.** `wav` → `audio/wav`, `mp3` / `mpeg` → `audio/mp3`, `m4a` / `mp4` → `audio/mp4`, `aac` → `audio/aac`, `flac` → `audio/flac`, `ogg` / `opus` → `audio/ogg`, `aiff` → `audio/aiff`. `m4a` is AAC in an MP4 container — what every Apple recorder produces — and the Gemini backend reads it as `audio/mp4`, so it does **not** map to `audio/aac`. An unrecognized `format` with no data-URL mime to fall back on is `400 unsupported_audio_format`, never a guessed mime — a wrong mime is a silent upstream misread. Passthrough providers (`grok`, custom-openai) never meet this validation: their body goes out as the client wrote it.

**`/anthropic` has no audio.** The Messages wire format defines no audio content block, so the Anthropic surface neither accepts nor converts audio — including for `antigravity`, whose Gemini upstream would happily take it. Audio goes on `/openai/v1`.

**Audio output is still out of scope**: no TTS, no `modalities: ["audio"]`. An inline audio part appearing in a *response* is surfaced the way an inline image is, as a data URI.

### Custom providers (`slug/upstream`, a user-defined endpoint)

`model` may also be `<slug>/<upstream_id>` for a user-defined custom endpoint (`docs/providers.md`). Resolution: split on the first `/`; if the prefix is a builtin `ProviderId` use the table above; otherwise look up `custom_providers` for the **authenticated user** by that slug — a miss (unknown slug, or a slug that belongs to a different user) is the same `400 invalid_model` as an unknown builtin. Behavior on `/openai/v1/chat/completions` depends on the provider's `format`:

| Custom `format` | Behavior |
|------------------|----------|
| `openai` | Near-passthrough to `{base_url}/chat/completions`: `model` rewritten to the bare upstream id, the rest of the client body forwarded **verbatim** — including `temperature`, `reasoning_effort` (unclamped, no provider ceiling), `response_format`, and any other field. This is a deliberate divergence from every built-in adapter. After a verbatim first send, a recognized unsupported-effort 400 may rewrite only `reasoning_effort` and retry once on the same account — see "`reasoning_effort`" below. |
| `anthropic` | Converted via the same shared OpenAI↔Anthropic converters `claude-code` uses (`openaiToAnthropicMessages` / `anthropicToOpenAIResponse`), sent to `{base_url}/v1/messages`. `reasoning_effort` is **dropped** on this surface (no `thinking`/`output_config` synthesized) — use the native `/anthropic` surface for full `thinking` control. |

Neither custom format supports `prompt_cache_key`. See [providers.md](./providers.md) for the full endpoint-construction, auth-header, and SSRF-guard rules.

Streaming: SSE, OpenAI chunk shape, end-to-end without buffering entire completion.

### Codex request/response shape

- **System → `instructions`:** `role: "system"` messages are never sent as `input` items. Their text (in order, `"\n\n"`-joined for multiple system messages) becomes the top-level Responses `instructions` field instead; this applies whether the system message came from the OpenAI surface directly or from an Anthropic `system` block converted to an OpenAI system message on the `/anthropic` surface.
- **`store: false`:** every codex request sets `store: false` explicitly — the Responses API defaults to `store: true`, which this proxy does not want (no server-side conversation state is kept upstream).
- **`include: ["reasoning.encrypted_content"]`:** sent on every codex request, so the upstream actually returns its reasoning items. Combined with the reasoning replay cache (see [providers.md](./providers.md)), this is what lets a multi-turn agent keep its chain of thought across tool rounds — with `store: false` and no `include`, the upstream keeps no state and returns no reasoning, so every turn starts cold.
- **`parallel_tool_calls: true`** when the request carries `tools` (dropped when it does not — upstream rejects it without tools), so an agent can fan out several tool calls in one turn instead of serializing them.
- **`prompt_cache_key`:** forwarded when the client sends a non-empty string, after being **fitted to OpenAI's 64-character limit** — the Responses backend rejects anything longer with `400 Invalid 'prompt_cache_key': string too long`, and Claude Code's `user_<hash>_account_<uuid>_session_<uuid>` id is ~150 chars, so an unfitted key fails every `/anthropic` → `codex` turn. Fitting is deterministic, so the value stays stable for a conversation (which is the only property the upstream cache router needs): (1) a key ending in `_session_<uuid>` sends that bare 36-char uuid — the same value the Codex CLI puts in `session_id`; (2) any other key ≤ 64 chars is sent verbatim; (3) anything longer is sent as the SHA-256 hex of the key (exactly 64 chars) rather than truncated, so two long keys sharing a prefix cannot collide onto one cache shard. The **full** key is still what feeds the reasoning-replay session id and the `session_id` header — only the upstream body field is fitted. Account acquisition is deterministic (highest `priority`, then newest `created_at` — see [providers.md](./providers.md)), so the same upstream ChatGPT account normally serves every turn of a conversation, and a stable `prompt_cache_key` lets that account's upstream prompt cache actually hit. On the `/anthropic` surface — whose wire format has no such field — it is **derived from Anthropic `metadata.user_id`** (string, trimmed, non-empty, ≤ 256 chars — Anthropic's own limit — else no key): Claude Code sends `user_<hash>_account_<uuid>_session_<uuid>` there, stable for a whole session, which is exactly the per-conversation granularity OpenAI's cache routing wants. Without a stable key every Claude Code request hashes to the same shared static prefix (system prompt + tools) and whether a turn hits cache depends on which upstream shard it lands on. This is a client-supplied id translated between wire formats — never a server-invented one; no `metadata.user_id` ⇒ no derived key, exactly today's behavior. The key (client-sent or derived) additionally becomes the reasoning-replay session fallback (see [providers.md](./providers.md)) and feeds the `session_id` header: client affinity headers win when present, then the key's trailing `_session_<uuid>` when it matches Claude Code's shape, then the raw key, then a per-request random UUID as before. The key is never logged, and it is never injected into a custom-openai provider's forwarded body — that body stays the client's verbatim JSON.
- **`reasoning_content`:** when the upstream reasoning summary stream (`response.reasoning_summary_text.delta`) carries text, it is surfaced using the de-facto `reasoning_content` extension field (the DeepSeek/OpenRouter convention — there is no first-party OpenAI field for this): as `delta.reasoning_content` on streamed chunks, and as `message.reasoning_content` on the non-stream completion. On the `/anthropic` surface this converts into a leading, **unsigned** Anthropic `thinking` content block — the same mechanism grok's `reasoning_content` uses (see the Grok reasoning note above and "Thinking blocks on the conversion path" below).
- **Upstream failures mid-turn (`response.failed` / `error` events):** the ChatGPT backend can end a Responses SSE turn with a `response.failed` or `error` event instead of `response.completed` (rate limit, content policy, backend fault, etc.). This proxy never fabricates a `200` completion for that. Streaming: a single OpenAI-shaped error line (`data: {"error":{"message","type":"upstream_error"}}`) replaces the rest of the turn and the stream ends immediately — no `finish_reason` chunk, no `[DONE]`. Non-stream: the adapter returns `502` with `{"error":{"message","type":"upstream_error"}}` instead of a completion built from a partial/empty turn. On the `/anthropic` surface, the same failure converts to an Anthropic `event: error` (`{"type":"error","error":{"type":"api_error","message"}}`) and the stream ends there.

### Claude Code streaming usage

`claude-code` requests made through the OpenAI surface (`stream: true`) convert the upstream Anthropic Messages SSE into OpenAI chunks. Usage is not known until the upstream stream reports it, so — like the codex converter above — it rides on the **final** chunk rather than an early one: prompt tokens come from `message_start` (`usage.input_tokens` plus `cache_read_input_tokens` / `cache_creation_input_tokens`, summed the same way the non-stream response does), completion tokens from `message_delta.usage.output_tokens`, and the combined `{prompt_tokens, completion_tokens, total_tokens}` is attached to the last chunk (the one carrying `finish_reason`) whenever any of those counts were seen. An upstream Anthropic `event: error` mid-stream converts to an OpenAI-shaped error line and ends the stream — no trailing `finish_reason` chunk, no `[DONE]`.

### Usage cache details on converted responses

Wherever this proxy **builds** an OpenAI-shaped `usage` object (claude-code / custom-anthropic conversions, codex Responses conversions — stream final chunk and non-stream alike), it attaches the upstream cache numbers instead of discarding them: `prompt_tokens_details.cached_tokens` (official OpenAI field; from Anthropic `cache_read_input_tokens` or Responses `input_tokens_details.cached_tokens`) and, for Anthropic-shaped upstreams only, a `cache_creation_input_tokens` extension field alongside it. `prompt_tokens` remains the cache-inclusive total.

Conversely, when building an Anthropic-shaped `usage` from an OpenAI upstream that reported `prompt_tokens_details.cached_tokens` (`/anthropic` → grok / codex / custom `format=openai`), the converted usage reports proper Anthropic semantics: `input_tokens` excludes the cached share, which appears as `cache_read_input_tokens`.

Pure passthrough paths (`grok` and custom `format=openai` on `/openai/v1`; native `/anthropic` passthroughs) never rewrite upstream usage. These numbers also feed `request_logs` for the admin dashboard (see [logging.md](./logging.md)).

### `POST /openai/v1/responses`

OpenAI **Responses API** ingress — the wire the Codex CLI speaks (`wire_api = "responses"` has been its only value since early 2026; see the docs-site Codex CLI page) and what OpenAI SDK `client.responses.create` sends. Same auth, model ids, group mounts (`/g/<slug>/openai/v1/responses`), routing, failover, spend limits, loop guard, and logging as Chat Completions. Two paths, decided per request from the resolved candidate list:

**Native path — every candidate is `codex`.** The client's Responses body goes upstream to `/codex/responses` as-is, after the same fix-ups the Chat path applies (see "Codex request/response shape"): `model` rewritten to the upstream id; `store: false` and `stream: true` forced; `role: "system"` input items hoisted into `instructions` (appended after the client's own `instructions`, `"\n\n"`-joined); `include` guaranteed to carry `reasoning.encrypted_content`; `reasoning.effort` validated and clamped to `xhigh`; `call_id`s over 64 chars shortened consistently across `function_call` / `function_call_output` / `custom_tool_call` / `custom_tool_call_output`; `prompt_cache_key` fitted to 64 chars (and feeding the `session_id` header as on the Chat path); `tool_choice` dropped when there are no `tools`; `service_tier` kept only when `"priority"`; the rejected-field list stripped. Everything else — `input` items including client-echoed `reasoning` items with `encrypted_content`, hosted tools such as `web_search`, `namespace` tool groups, `text`, `parallel_tool_calls`, `client_metadata` — passes through untouched, so a Codex CLI turn reaches the ChatGPT backend in the shape the CLI itself would send. The upstream SSE is relayed byte-for-byte (`event:` lines included). The **reasoning replay cache is not used** here: a Responses client echoes the previous turn's reasoning items itself, as the Codex CLI does, and the proxy must not inject a second copy. `response.model` echoes the upstream model id, same as the Chat path's converted chunks. In-stream failures raised by dispatch itself (pool exhaustion, upstream non-2xx, stall — see "In-stream errors") are rewritten from the OpenAI error line into a `response.failed` event so a Responses client sees a well-formed terminal event. **Non-stream** (`stream: false`): the SSE is collected and the `response.completed` event's `response` object is returned as JSON; a `response.failed` / `error` turn returns `502 {"error":{"message","type":"upstream_error"}}`, never a fabricated success.

**Conversion path — any other target** (`claude-code`, `grok`, `antigravity`, custom providers, or a group mixing codex with anything else). The request converts to the internal Chat Completions shape, dispatches exactly like `/openai/v1/chat/completions` (every adapter unchanged — a codex target inside a mixed group goes through the existing Chat → Responses adapter, replay cache included), and the Chat result converts back to Responses events / object. A `claude-code` or custom `format=anthropic` target gets the proxy-placed `cache_control` breakpoints described under "Prompt cache" — the Codex CLI resends the whole conversation every tool round, so without them every turn is a full-price prompt.

| Responses field | Chat Completions |
|---|---|
| `instructions` | leading `system` message |
| `input` string | one `user` message |
| `message` items (`user` / `assistant` / `developer` / `system`; the `{role, content}` shorthand too) | same-role messages, `developer` → `system`; `input_text` / `output_text` / `refusal` → text parts; `input_image` (`image_url` string, or `{image_url, detail}`) → `image_url` part |
| `function_call` | assistant `tool_calls` entry (`id` = `call_id`); consecutive calls, and an assistant text immediately before them, merge into one assistant message |
| `function_call_output` | `role: "tool"` message (`tool_call_id` = `call_id`); text parts joined, non-text parts dropped |
| `custom_tool_call` / `custom_tool_call_output` | as the function items, with arguments `{"input": <string>}` |
| `reasoning` items | dropped — Chat has no carrier, and the `encrypted_content` belongs to a different provider anyway |
| hosted call items (`web_search_call`, …) | dropped |
| `item_reference` | `400 unsupported_field` (needs server-side state) |
| `tools[].type = "function"` | `{type:"function", function:{name, description, parameters, strict}}` |
| `tools[].type = "namespace"` | flattened: each inner tool becomes a function named `<namespace>__<name>`; a call to it converts back into a `function_call` carrying `namespace` and the bare `name` — the two fields the Codex CLI resolves a tool by |
| `tools[].type = "custom"` | function with a single required string parameter `input` |
| `tools[].type = "web_search"` / `"web_search_preview"` | replaced by a plain function tool `web_search` whose description says web search is unavailable for this model (see below) |
| other hosted tools (`image_generation`, `file_search`, `code_interpreter`, `computer_use_preview`, `mcp`, `local_shell`, `shell`) | dropped |
| `tool_choice` | `auto` / `none` / `required` verbatim; `{type:"function", name}` → nested shape (flattened name); `allowed_tools` and hosted-tool choices → `auto` |
| `text.format` `json_schema` / `json_object` | `response_format` |
| `text.verbosity` | dropped |
| `reasoning.effort` | `reasoning_effort` (validated as on Chat; `reasoning.summary` ignored) |
| `max_output_tokens` | `max_tokens` |
| `temperature`, `top_p`, `stream`, `prompt_cache_key`, `parallel_tool_calls` | same field |
| `previous_response_id`, `conversation`, `background: true` | `400 unsupported_field` — this proxy stores nothing upstream (`store: false` everywhere), so there is no state to continue from |
| `store`, `include`, `truncation`, `metadata`, `user`, `safety_identifier`, `service_tier`, `client_metadata` | dropped |

Unsupported input-content part types (`input_file`, `input_audio`) are `400 unsupported_field` rather than silently dropped, for the same reason the audio guard above rejects rather than answers as if the part were silence.

**Web search on the conversion path.** The Codex CLI attaches a hosted `web_search` tool to every request by default (`web_search = "cached"` is its default mode, and its provider capabilities default `web_search: true` for custom providers). Rejecting the request would break the CLI out of the box; silently dropping the tool would leave the model unaware it ever had one. The proxy instead exposes a **stub function tool** named `web_search` whose description states that web search is not available for this model through the proxy. If the model calls it anyway, the **client** answers, not the proxy: the Codex CLI feeds `unsupported call: web_search` back to the model as the tool result and the agent loop continues (codex-rs `core/src/tools/registry.rs`). The proxy never fabricates a tool result or a model message. Setting `web_search = "disabled"` in the Codex config removes the tool entirely.

**Output conversion (stream).** Chat chunks become the standard Responses event sequence, each event carrying a `sequence_number`: `response.created` → `response.in_progress` → per output item `response.output_item.added` … `response.output_item.done` → `response.completed`. `delta.reasoning_content` becomes a `reasoning` item with a `summary_text` part (`response.reasoning_summary_part.added` / `response.reasoning_summary_text.delta` / `…text.done` / `…part.done`); `delta.content` a `message` item with an `output_text` part (`response.content_part.added` / `response.output_text.delta` / `…text.done` / `…part.done`); `delta.tool_calls` a `function_call` item (`response.function_call_arguments.delta` / `…done`) — or a `custom_tool_call` item for a `custom` tool, emitted whole when the turn ends. Chat upstreams stream tool calls one at a time, so a new tool-call header closes the previous `function_call` item; an argument delta arriving for an already-closed item cannot be re-streamed and is dropped. Item ids (`rs_…`, `msg_…`, `fc_…`, `ctc_…`) and the response id (`resp_…`) are generated per response — protocol chrome, not product data. `finish_reason: "length"` ends as `response.incomplete` with `incomplete_details.reason: "max_output_tokens"`. Usage from the final chunk maps to `usage.input_tokens` / `output_tokens` / `total_tokens`, with `input_tokens_details.cached_tokens` and `output_tokens_details.reasoning_tokens` when reported (absent otherwise, never zero-filled). An in-stream OpenAI error line (upstream failure, pool exhaustion, stall frame — see "In-stream errors") becomes `response.failed` with `response.error.{code, message}` and ends the stream. Keepalive comments are re-applied on the converted stream. **Non-stream:** the Chat completion object converts to one Response object with the same item shapes; HTTP errors keep their status and the OpenAI error envelope.

`GET /openai/v1/responses/{id}`, `DELETE`, `cancel`, `input_items`, and `compact` do not exist: nothing is stored, so there is nothing to fetch. The Codex CLI does not need them against a custom provider — its REST path never sends `previous_response_id` (only its WebSocket transport does, and custom providers default to `supports_websockets = false`), and remote compaction is only enabled for OpenAI's own endpoints, so it compacts locally.

### `GET /openai/v1/models`

Returns OpenAI-style `{ object: "list", data: [...] }` for providers the key owner has bound. Ids are `provider/upstream_id`. Model groups are **not** listed here (since v4 they live on their own endpoints — each group's `GET /g/<slug>/openai/v1/models` lists its model names; see "Group endpoints"). Claude Code and Grok come from live upstream `/models`; Antigravity from `v1internal:fetchAvailableModels`, ids verbatim and empty on failure (see [providers.md](./providers.md) § Antigravity). Codex comes from the ChatGPT `backend-api/codex/models` endpoint, falling back to the public catalog mirror when the edge bot-walls the Worker (see [providers.md](./providers.md)). Empty for a provider when the user has no usable account for it. The user's custom providers are appended after the builtins — manual list, or live + cache + fallback for `models_mode=auto` (see [providers.md](./providers.md)).

### `POST /openai/v1/audio/transcriptions`

OpenAI-compatible Speech-to-Text audio transcription endpoint.

Request shape: `multipart/form-data`
- `file`: audio file binary (required)
- `model`: required. `provider/model` on this shared base; on a group endpoint, one of that group's model names (see "Model routing" and "Group endpoints")
- `language`: optional ISO-639-1 code
- `prompt`: optional text
- `response_format`: optional (`json`, `text`, `srt`, `verbose_json`, `vtt`), default `json`
- `temperature`: optional float
- `timestamp_granularities[]`: optional array (`word`, `segment`)

**Provider support:**
- **Custom providers with `format=openai`:** `model` is rewritten to the bare upstream model id; `file` and all other form fields are forwarded verbatim to `{base_url}/audio/transcriptions`. Upstream responses (JSON, verbose JSON, plain text, SRT, VTT) and HTTP statuses pass through directly.
- **Built-in subscription providers (`claude-code`, `codex`, `grok`, `antigravity`) and custom providers with `format=anthropic`:** rejected with `400 unsupported_modality` ("audio transcription is not supported by \"<provider>\" — only custom OpenAI-format providers support the audio/transcriptions endpoint").

## Anthropic surface

Same providers as the OpenAI surface. Model id is always `provider/upstream` (not bare).

### `POST /anthropic/v1/messages`

| `model` provider | Behavior |
|------------------|----------|
| `claude-code` | Anthropic Messages **passthrough** to Claude Code OAuth upstream: auth inject, fixed Claude Code system prepend if missing (identical string every time), `anthropic-beta` = the client's list **verbatim** plus only the two OAuth-required betas (see [providers.md](./providers.md) "Base betas"), **`cache_control` never rewritten** (block- or top-level). Upstream `model` field is the bare id after the prefix. A retired top-level `output_format` is moved to `output_config.format` when the body has no `output_config.format` of its own (Anthropic now `400`s the old spelling; clients built on older SDKs still send it). When the (patched) request body contains an `output_config` key, `effort-2025-11-24` is added to the outgoing `anthropic-beta` header automatically (deduped against client-supplied betas) so upstream honors `output_config.effort`; clients that already send that beta are not double-added. |
| `grok` | Anthropic Messages → xAI **Responses** (`cli-chat-proxy.grok.com/v1/responses`) → Anthropic Messages. Streaming conversion includes **text, signed `thinking`, and `tool_use`**. `thinking.signature` ↔ Responses `reasoning.encrypted_content`; stream emits `signature_delta`. Tools / vision / structured output (`output_config.format` json_schema, or the retired `output_format` spelling — both read) convert; **`stop_sequences` is dropped** (Responses has no Chat Completions `stop` equivalent — same as codex). Anthropic `cache_control` stripped. Optional client headers `x-grok-conv-id` / `x-grok-session-id` / `x-grok-turn-idx` forwarded when present; never synthesized. Thinking/effort rules and the reasoning replay cache: see "Grok reasoning" above and [providers.md](./providers.md). The degenerate tool-call loop guard still applies (this is a conversion path, not Claude-native passthrough). |
| `codex` | Convert Messages → internal Chat Completions shape → existing provider adapter → convert response/SSE back to Anthropic Messages. Streaming conversion includes **text and `tool_use`** (`input_json_delta` from OpenAI `tool_calls` argument chunks) so Claude Code / CC Switch can complete tool rounds. The codex streaming converter maps Responses events (`response.output_item.added` → tool_call header, `response.function_call_arguments.delta` → argument fragments, `response.output_item.done` as fallback when no deltas were seen) onto OpenAI `tool_calls` chunks, with `call_id` as the tool id so replayed history matches `function_call_output.call_id`. `stop_sequences` forwards as OpenAI `stop`; `system` blocks are joined with a blank line. Anthropic `cache_control` has no equivalent → **stripped on convert**; instead, client `metadata.user_id` is translated to the Responses `prompt_cache_key` / stable `session_id` for upstream cache affinity (see "Prompt cache" below and "Codex Responses details"). |
| `antigravity` | Anthropic Messages → Gemini `v1internal:generateContent` → Anthropic Messages. Converts **text, `thinking` (Gemini `thoughtSignature` ↔ `thinking.signature`, stream emits `signature_delta`), `tool_use`/`tool_result`, base64 images, `stop_sequences` (→ `stopSequences`)** and the sampling fields. Anthropic `cache_control` has no Gemini equivalent → **stripped on convert**. A tool call arrives as one complete `args` object per upstream frame, so its `input_json_delta` is emitted whole rather than in fragments. Client `metadata.user_id` becomes the CloudCode `sessionId` for shard affinity; never synthesized from message content when the client supplied one. The degenerate tool-call loop guard applies (conversion path, not Claude-native passthrough). |
| custom, `format=anthropic` | Native **passthrough** to `{base_url}/v1/messages`, same shape as `claude-code` (auth inject, `model` rewritten to the bare upstream id, `cache_control`/`thinking` never touched) but with **none** of the Claude-Code-OAuth specifics: no system prepend, no auto-added effort beta, no fixed base betas — `anthropic-beta` is forwarded verbatim from the client (or omitted) and `anthropic-version` defaults to `2023-06-01` only when the client sends none. |
| custom, `format=openai` | Same conversion path as `grok`/`codex` (`cache_control` stripped, tool/vision conversion identical), landing on the custom-openai adapter's `chatCompletions()` instead of a builtin one. |

`model` **must** be `provider/upstream` (e.g. `claude-code/claude-opus-5`, `grok/grok-4.5`, or `<slug>/<upstream>` for a custom endpoint). Any bare id → `400` `invalid_model` — bare model-group aliases no longer resolve on this shared base (since v4 a group is called through its own endpoint; see "Group endpoints"). A slug that doesn't match one of the caller's own custom providers is the same `400 invalid_model` as an unknown builtin — a custom slug never resolves cross-user.

Converted streams follow the Anthropic content-block contract: blocks are strictly
sequential (one open at a time, dense ascending indices, never reopened). Because an
OpenAI chunk stream may interleave text with a call's `arguments` — and may alternate
fragments between several `tool_calls[].index` values — the converter streams the first
tool call live, buffers interleaved text into its own block, and emits any further calls
complete at the end of the turn. `usage` is taken from the upstream final chunk
(`stream_options.include_usage`) when the provider reports it.

**Server-side tools are dropped, not forwarded.** Anthropic tool definitions that carry a `type` other than `"custom"` and no `input_schema` (`web_search_*`, `bash_*`, `text_editor_*`, `computer_*`, `code_execution_*`, and future Anthropic-hosted tools) have no `grok`/`codex`/`antigravity` equivalent — neither backend can execute them. The converter drops these tools entirely instead of forwarding a fake empty-schema function, which would otherwise invite an uncallable tool call. Client-defined tools (anything with `input_schema`, or no `type` / `type: "custom"`) still convert to OpenAI function tools; anything already OpenAI-shaped (`{function: {...}}`) passes through unchanged.

**Images inside `tool_result` are re-attached as a follow-up message.** `grok`/`codex` have no Anthropic-style multi-part `tool` message; when a `tool_result` block's content contains `image` blocks, the converted `role: "tool"` message keeps the text parts plus a short placeholder line, and is immediately followed by one `role: "user"` message carrying the image(s) as `image_url` parts (same base64 → data URI / URL conversion used for regular user content). A `tool_result` with no images converts as before (text only).

**Thinking blocks on the conversion path.**

- **`/anthropic` → `grok` (Responses):** leading `thinking` block **with** `signature` when upstream returned `encrypted_content` (stream: `thinking_delta` + `signature_delta` before `content_block_stop`). No signature is invented — absent encrypted content ⇒ no signature field. Round-trip: assistant `thinking.signature` → Responses `reasoning` input `encrypted_content`; thinking plaintext is not required for replay. `redacted_thinking` dropped. When `thinking.type=disabled`, no thinking blocks are emitted even if upstream sent reasoning.
- **`/anthropic` → `antigravity` (Gemini convert):** `thinking` block **with** `signature` whenever the upstream part carried a `thoughtSignature`; nothing is invented when it did not. The signature is echoed back as `thoughtSignature` on the next turn, which is what keeps multi-turn thinking valid — so unlike grok/codex there is no replay cache, the client can carry it itself. `thinking.type=disabled` suppresses the blocks entirely.
- **`/anthropic` → `codex` / custom-openai (Chat Completions convert):** unsigned thinking from de-facto `reasoning_content` (`{type:"thinking", thinking:"..."}` — no `signature`). Streamed live when reasoning precedes text/tools; late fragments are buffered and flushed as one complete block at end of turn. Round-trip: thinking text → `reasoning_content` on the OpenAI-shaped assistant message; codex's Responses builder still ignores that plaintext field (the real chain of thought travels as opaque `encrypted_content` through the replay cache instead — see [providers.md](./providers.md)); custom-openai forwards it verbatim.

### `POST /anthropic/v1/messages/count_tokens`

Parses the body the same way as `/v1/messages` — same `model` requirement, same `400` envelopes for invalid JSON / invalid model id. On a group endpoint (`/g/<slug>/anthropic/v1/messages/count_tokens`) the group model expands first and follows its resolved target's row below.

| `model` provider | Behavior |
|------------------|----------|
| `claude-code` | Passthrough to upstream `https://api.anthropic.com/v1/messages/count_tokens` with the same header construction as `/v1/messages` (OAuth bearer, `anthropic-version` default `2023-06-01`, beta header via `resolveBetaHeader`), the bare upstream model id, and the fixed Claude Code system prepend applied (idempotent — Claude Code clients already send that exact first system block). Reuses the same account pool / bench-on-401/402/403/429 failover loop as `/v1/messages`. Never streams — always a non-stream JSON response, returned as-is on success or upstream error. |
| `antigravity` | Converts the body to Gemini and calls `v1internal:countTokens` with a bare `{"request": <gemini request>}` body — no CloudCode generate envelope (`userAgent`/`requestType`/`requestId`/`sessionId`) and no Claude `VALIDATED` toolConfig injection, all of which that endpoint rejects with a 400 (see [providers.md](./providers.md) § Antigravity). Answers `{"input_tokens": <totalTokens>}`. A **real upstream count**, not a local estimate (the endpoint ignores `tools` when counting, so tool schemas are undercounted). Same pool / failover loop. |
| `codex` | `200` with a **relay-computed tiktoken count**: the Worker serializes the body's text (system, message blocks, tool names/descriptions/schemas) and POSTs it to the egress relay's `/count-tokens`, which runs the public OpenAI `o200k_base` tokenizer locally ([codex-relay.md](./codex-relay.md) § Token counting). No ChatGPT-backend call, no account acquired, no tokens consumed. This is a real tokenizer over an approximate serialization — the backend's own prompt framing adds a few percent the count cannot see. **Any relay failure (unconfigured, unreachable, non-200, timeout) degrades to `{"input_tokens": 0}`** — never an error status, because a failed count_tokens makes Claude Code fall back to probing with real `max_tokens: 1` generation requests in parallel (measured 2026-08-22; see the sentinel-zero row below). |
| `grok` | `200` with the **sentinel `{"input_tokens": 0}`**, no upstream call. xAI's subscription backend has no token-counting endpoint and no public tokenizer. Zero is deliberate: it is unmistakably "no data", where a local heuristic (chars/4) would be a plausible-looking fabricated number. Returning `400` here is worse than either — the client's fallback fires ~15–20 real `max_tokens: 1` generation requests in parallel, which burned real quota and (on Antigravity) tripped a provider-side account suspension (measured 2026-08-22). Quietly wrong display beats blasting the upstream — operator decision. |
| custom, `format=anthropic` | Passthrough to `{base_url}/v1/messages/count_tokens`, same header construction (and same OAuth-specifics omissions) as that provider's `/v1/messages`. Reuses the same account pool / failover loop. |
| custom, `format=openai` | With `count_tokens_url` set: forwarded there ([providers.md](./providers.md)). Without it: the same sentinel `{"input_tokens": 0}` as `grok`, same rationale. |

Every locally answered row (codex relay-counted or degraded, grok, custom-openai without URL) still writes a `request_logs` row — `error_code: "count_tokens_stub"` when the answer was the sentinel zero, `NULL` when a relay count succeeded ([logging.md](./logging.md)).

### `GET /anthropic/v1/models`

Same live catalog as `GET /openai/v1/models` for the key owner: all providers with usable accounts, including the user's custom providers appended after the builtins. Model groups are not listed (see "Group endpoints"). Envelope is Anthropic-ish `{ data: [{ id, display_name, type: "model" }] }` with `id` = `provider/upstream`.

### Future

Same host keeps `/anthropic/*` for additional Anthropic routes if needed; do not break base URL clients already use.

## Group endpoints (`/g/<slug>/…`)

Every model group ([providers.md](./providers.md) § Model groups) is its own virtual endpoint, in both wire shapes:

| Route | Mirrors |
|-------|---------|
| `POST /g/<slug>/openai/v1/chat/completions` | `POST /openai/v1/chat/completions` |
| `POST /g/<slug>/openai/v1/responses` | `POST /openai/v1/responses` |
| `POST /g/<slug>/openai/v1/audio/transcriptions` | `POST /openai/v1/audio/transcriptions` |
| `GET /g/<slug>/openai/v1/models` | `GET /openai/v1/models` |
| `POST /g/<slug>/anthropic/v1/messages` | `POST /anthropic/v1/messages` |
| `POST /g/<slug>/anthropic/v1/messages/count_tokens` | `POST /anthropic/v1/messages/count_tokens` |
| `GET /g/<slug>/anthropic/v1/models` | `GET /anthropic/v1/models` |

- **Auth is unchanged:** the same project-issued API keys, either header. The slug is resolved **scoped to the key's owner** — another user's slug is a 404, exactly like a cross-user custom slug on the shared base.
- **Unknown slug → `404`** (surface-shaped error envelope): the endpoint does not exist. Unknown `model` on a known slug → `400 invalid_model`, same envelope as the shared base but the message points at the group's configured model names.
- **The group is a closed mapping table:** `model` must exactly match one of the group's model names — including a name that happens to look like `provider/model` only if the group defines that literal string. There is no fallthrough to the shared base's `provider/model` resolution.
- **`GET …/models`** (both shapes) lists exactly that group's model names, `owned_by`/section label `group`, regardless of current target usability.
- Everything after resolution — dispatch, failover, streaming, errors, count_tokens per-provider behavior — follows the expanded target's provider exactly as if the client had sent that `provider/model` on the shared base. Response/stream `model` fields echo the name the client sent. `request_logs` stores the expanded canonical id plus `group_name` = `<slug>/<model name>` ([database.md](./database.md)).

## Model routing

**Group endpoints** resolve first by path: the slug names the group (404 when it isn't the caller's), the request's `model` names one of its models (exact match; miss → `400 invalid_model`), and that model's targets expand into the flat candidate list per the group's `strategy` (default `ordered` — full contract in [providers.md](./providers.md) § Model groups and § Routing module). A target pinned to a specific account contributes exactly that account as a candidate; on a bench-type failure the walk continues into the model's later targets within the same request. Steps 4–6 below then apply to the expanded list unchanged.

**Shared bases** (`/openai/v1`, `/anthropic`):

1. A `model` string **without any `/`** is `400 invalid_model` — since v4 nothing bare resolves here (group models live on their own endpoints).
2. Otherwise parse `provider` from `model` (`provider/rest` → provider, rest = upstream model id, split on the **first** `/` only — an upstream id may itself contain further `/`).
3. If `provider` is a builtin `ProviderId`, use it directly. Otherwise look it up as a custom provider slug, scoped to the authenticated user (`custom_providers` table) — never resolves another user's slug.
4. Resolve user’s pool for that provider (or custom slug).
5. Walk the routing module's candidate list ([providers.md](./providers.md) § Routing module) under the pool's `strategy` (default `ordered` = pool priority). Candidates whose stored usage snapshot shows an exhausted window (`utilization ≥ 100`) are skipped up front until that window's `resets_at`. On upstream 401/402/403/429/520/522/524, bench and try the next candidate — 429 benches until the upstream reset when derivable (for `antigravity` that reset is classified out of the response body, see [providers.md](./providers.md) § Antigravity), others 300s (402 = billing/credit exhaustion — e.g. OpenRouter's `402 Insufficient credits` — the account is unusable until topped up, so retrying it per-request just burns a failing upstream round-trip).
6. No usable account → error (below).
7. No provider match at all (not a builtin id, not one of the caller's custom slugs) → `400 invalid_model`.

A group-expanded request is indistinguishable from a direct one past resolution: the resolved provider's own rules (reasoning ceiling, prompt cache, betas, loop guard, `count_tokens` support) all follow the **target**, not the group. Client-visible `model` fields echo the group model name the client sent; `request_logs` stores the expanded canonical id plus `group_name` ([database.md](./database.md)).

## `reasoning_effort`

Client field (OpenAI body; Anthropic may use same via extension or map from omitted):

`none` | `low` | `medium` | `high` | `xhigh` | `max` | omit

| Provider | Mapping | Ceiling |
|----------|---------|---------|
| grok | `/openai/v1`: top-level Chat Completions `reasoning_effort`; omit if unset. `/anthropic`: Responses `reasoning.effort` from `output_config.effort` / `reasoning_effort` / thinking mode (see "Grok reasoning"); **omit `reasoning` entirely** when `thinking.type=disabled` | `xhigh` |
| codex | Omit field if none/unset; else Responses `reasoning: { effort, summary: "auto" }` | `xhigh` |
| claude-code | Map to `output_config.effort`; off/`none` → thinking disabled + safe effort; **no public `thinking: adaptive` API** | `max` |
| antigravity | Gemini `generationConfig.thinkingConfig.thinkingLevel`; `none` → `thinkingBudget: 0` (there is no "none" level). `includeThoughts` is on unless thinking is disabled | `high` |
| custom, `format=openai` | Forwarded verbatim as top-level `reasoning_effort` (part of the near-passthrough body). **Not** ceiling-clamped. One post-reject remap, below | **none — never clamped** |
| custom, `format=anthropic` | **Dropped** on `/openai/v1` (no `thinking`/`output_config` synthesized); on native `/anthropic`, whatever `thinking` object the client sends goes through untouched — full control, no ceiling | n/a |

**Ceiling clamp — a valid effort above a *builtin* provider's ceiling is lowered to that provider's highest supported effort, never rejected.** Example: Claude Code running at `max` with sonnet/haiku remapped to `grok/...` or `codex/...` sends `xhigh` upstream instead of getting a `400`. Ceilings verified 2026-08-02: xAI documents `high` as grok-4.5's top and `xhigh` for grok-4.20-multi-agent, and live grok-4.5 accepts `xhigh` without error (reasoning tokens scale up), so grok's provider-wide ceiling is `xhigh`; codex Responses models (gpt-5.2-codex, gpt-5.1-codex-max) top out at `xhigh` — `max` exists only on non-codex GPT-5.6 models, which this adapter does not serve. **Custom providers are never clamped on the first send** — there is no known upstream ceiling for an arbitrary BYO endpoint, so a custom-openai provider forwards whatever the client sent (already validated against the ladder below) as-is. Builtins are unchanged by the remap below.

**Custom-openai unsupported-effort remap (same account, once).** Some BYO OpenAI-compatible servers apply a Jinja chat template that rejects a *valid* ladder token the first send just forwarded. Observed 2026-08-16: Tabby serving Qwen 3.8 27B returns HTTP 400 JSON `{"detail":"TemplateError: Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low."}` — `high` is on our ladder and is legal on official Grok / Codex / Claude, but that template's allowed set is `{xhigh, medium, low}`. Builtin adapters must not remap this: official Grok accepts `high` (and often defaults to it). The recovery lives only in the custom-openai adapter:

1. First `POST {base}/chat/completions` is still verbatim (model rewrite only).
2. Remap is considered only on **HTTP 400 whose body is not an event-stream**. A 2xx (including a started SSE body) is never read for this purpose and is returned as-is.
3. The 400 text is handed to an ordered list of **effort-rejection parsers** (`providers/custom_openai_reasoning.ts`). The first parser that recognizes the body wins and returns `{rejected, allowed}` (`rejected` / each `allowed` entry is a ladder token). Parsers that do not match return `null`. Unrecognized 400s — including every non-TemplateError — are reconstructed from the already-read text and returned unchanged. Adding another vendor's wording is a new parser on that list, not a change to the first-send or retry policy.
4. First registered parser (2026-08-16): Tabby + Qwen 3.8 Jinja `TemplateError` / `Unexpected reasoning effort <token>. Supported types are …`. Matches anywhere in the body text so FastAPI `{"detail":"…"}` and a bare exception string both work. `(default)` markers in the supported list are ignored.
5. `nearestReasoningEffort(rejected, allowed)` (in `utils/reasoning.ts`) picks the allowed token closest on `none < low < medium < high < xhigh < max`. **Equal distance prefers the higher token** — so `high` against `{low, medium, xhigh}` is `xhigh` (the client already asked for deeper than `medium`; this template's own default is `xhigh`). If `rejected` is already in `allowed`, or the mapped token equals the `reasoning_effort` we already sent, do not retry.
6. At most **one** retry, same account, same adapter `fetch`, same URL / auth / other body fields: only `reasoning_effort` is rewritten to the mapped token. Not a failover step, not a new acquire, not a group-target switch.
7. If the retry is not 2xx, return that retry response (do not fall back to the original 400). The original 400 body is discarded only after a remap is actually issued.

Does not apply to custom `format=anthropic` (`reasoning_effort` is dropped on `/openai/v1`; native `/anthropic` is passthrough). Does not apply to `grok` / `codex` / `claude-code`.

Only unknown tokens (anything outside the ladder above) → `400` via `parseReasoningEffort`, for every provider including custom ones — the ladder validation happens before routing to any adapter. Unknown model ids: pass the (clamped, for builtins) effort through when possible.

**Reasoning is effort-only for every provider except `antigravity`**, whose upstream `thinkingConfig` takes a native token budget and therefore does map `thinking.budget_tokens`. Provider handling of the Anthropic `thinking` object:

| Path | `thinking` object |
|------|-------------------|
| `/anthropic` → `claude-code` (and custom `format=anthropic`) | Passed through **untouched** |
| `/anthropic` → `grok` | **Honored** for enable/disable + adaptive/enabled mode; effort from `output_config.effort` / `reasoning_effort` (see "Grok reasoning"). `budget_tokens` ignored |
| `/anthropic` → `antigravity` | **Honored**: `type: "disabled"` → `thinkingBudget: 0`; `type: "enabled"` + `budget_tokens` → `thinkingBudget` verbatim — the one provider where `budget_tokens` **is** mapped, because Gemini's `thinkingConfig` takes a token budget natively; otherwise effort → `thinkingLevel` |
| `/anthropic` → `codex` / custom-openai | Dropped — that Chat Completions convert only reads `reasoning_effort` / `output_config.effort` |

Effort inputs, in priority order where applicable: (1) `reasoning_effort` (OpenAI field, also accepted on an Anthropic body); (2) `output_config.effort` on the Anthropic surface. Invalid values still `400` via `parseReasoningEffort`.

## Prompt cache

| Path | Policy |
|------|--------|
| `/anthropic` → `claude-code` | **Strict passthrough** of all client `cache_control`. Do not reorder tools/system/messages or normalize away cache-relevant structure. Fixed system prepend is byte-stable when added. |
| `/openai/v1` → `claude-code` (Chat Completions, and the `/openai/v1/responses` conversion path) | **Proxy-placed breakpoints (since 2026-09-05).** The OpenAI wire has no `cache_control`, so the converted body gets up to **four** `ephemeral` markers — Anthropic's per-request maximum — placed the way an agent loop wants them: (1) the **last tool definition** and (2) the **last `system` block** (the fixed Claude Code prepend is already in place when the marker is added), both `ttl: "1h"` — tools render before system, so these two entries cover the static prefix and should survive a human pause; (3) the **last cacheable block of the previous `user` turn** and (4) the **last cacheable block of the last message** (walking back past `thinking` / `redacted_thinking`; `text`, `image`, `document`, `tool_use`, `tool_result` qualify) — the standard multi-turn placement, each turn's breakpoint being the next turn's read point, with the previous turn kept as a fallback read point for a turn that appended more than the 20-position lookback or was edited. Message markers use `ttl: "1h"` when the client sent `prompt_cache_key` — an explicit declaration that the request belongs to a multi-request conversation; the Codex CLI sends its thread id there on every request — and the default 5 minutes otherwise, so a one-shot API caller pays the 1.25× write only on its own unique tail. Longer-TTL entries always precede shorter ones, as Anthropic requires. Explicit block markers only: never the top-level automatic field. Why: measured 2026-09-04, a Codex CLI → `claude-fable-5-1` session on `/openai/v1/responses` sent ~1.5M uncached input tokens in eight minutes with `cache_creation_input_tokens = 0` on every row, then benched the account. |
| `/anthropic` → `grok` | Strip `cache_control` on convert; no Anthropic-style breakpoints upstream. Sticky ids only from client `x-grok-*` headers — never derived. |
| `/anthropic` → `codex` | Strip `cache_control` on convert. Client `metadata.user_id` (non-empty string ≤ 256 chars) → Responses `prompt_cache_key` + stable `session_id` header, **both fitted to OpenAI's 64-char limit** (over-long on either one is a hard `400`), keeping a session's turns on one upstream cache shard (see "Codex Responses details"). A client-supplied id translated between wire formats — never invented server-side. |
| `/openai/v1` or `/anthropic` → `grok` | Forward client `x-grok-conv-id` (and session/turn) when supplied; **never invent**. Prefix cache works without conv-id (see [providers.md](./providers.md)). |
| `/anthropic` → custom, `format=anthropic` | **Strict passthrough** of all client `cache_control`, same as `claude-code` (no system prepend to keep byte-stable, though — there isn't one). |
| `/openai/v1` → custom, `format=anthropic` | Same four proxy-placed breakpoints and TTL rule as `claude-code` above (no system prepend, so the system marker lands on the client's own last system block; a request without tools or system gets only the message markers). |
| `/anthropic` → custom, `format=openai` | Strip `cache_control` on convert, same as `grok`/`codex`. |

## Streaming

Every streaming response, both surfaces, every provider, is relayed byte-for-byte from upstream — this proxy never buffers a whole completion before forwarding it — through a shared keepalive / eager-commit wrapper (`proxy/sse.ts`). Both wrappers are **demand-driven**: while the client is not consuming, the pump stops reading upstream (backpressure propagates instead of the remainder buffering in Worker memory), and the keepalive/idle timers pause with it — a client-side stall is not an upstream silence gap, so it neither draws keepalives into an unread queue nor counts toward the 120s upstream idle timeout.

### `message_start` and the client's context indicator

On the `/anthropic` surface, Anthropic clients read the **context size** off `message_start.usage.input_tokens` — Claude Code's `ctx` indicator is that field. The three conversion paths cannot always fill it, and what they do about that differs:

| Path | `message_start.input_tokens` |
|---|---|
| `claude-code` (native passthrough) | Upstream's own, untouched |
| **Gemini → Anthropic** (`antigravity`) | **Real.** The event is withheld until an upstream frame reports `promptTokenCount`, which Gemini carries on the same frame as the first content part (measured against CloudCode, 2026-08-22). If content is ready and no frame ever reported a count, the turn ends as an `error` event — a wrong context size is worse than a visible failure, and there is no honest number to substitute. An upstream that streams nothing at all therefore emits `error` with no preceding `message_start`. |
| OpenAI → Anthropic (`codex`, `grok`, custom) | **Real when the upstream reports usage before its first content chunk; `0` otherwise.** Usage is harvested from *every* chunk/event carrying it, not just the terminal one, and `message_start` is emitted lazily (first content block), so an early report lands in it. Measured against codex (`gpt-5.4-mini`, 2026-08-22) the Responses stream reports usage only on completion, so `0` is what those clients get; the same is expected of grok. Some OpenAI-compatible custom endpoints do emit usage on every chunk under `stream_options.include_usage`, and those now work. Neither codex nor grok exposes a `countTokens` method, so there is no second source to consult, and the stream is never buffered to wait for the final usage chunk. Do not invent a number here. |

Usage from later frames is merged **field-wise**, not replaced: Gemini repeats `promptTokenCount` on trailing frames without repeating `candidatesTokenCount`, and replacing the object wholesale zeroed the output count (it logged `completion_tokens: 0` for every Antigravity request through v3.12.1). `input_tokens` and `cache_read_input_tokens` move as a **pair** — taking a later frame's prompt count while keeping an earlier frame's cache number would count the cached tokens twice.

### Eager streaming commit

When the client requests `stream: true` (OpenAI body field, or Anthropic Messages body field — `count_tokens` never streams), the proxy **commits the HTTP response immediately**:

1. Returns `200` + `Content-Type: text/event-stream` **before** account acquire, token refresh, failover, or the upstream call.
2. Starts **keepalive comments from second 0** (not from first upstream byte).
3. Runs the pool/failover loop and upstream fetch **inside** the stream. Upstream TTFB of tens of seconds (large context prefill / long reasoning) no longer counts against the client's response-headers timeout — the client already has headers and is only waiting for tokens.
4. A client disconnect mid-wait cancels the stream (`onClose("cancel")`) so the row is logged as `client_abort` instead of vanishing with no D1 row (the pre-commit failure mode: Worker torn down while still `await`ing upstream headers).

**Non-stream** requests (`stream` absent/false) are unchanged: the proxy still waits for upstream headers and returns that HTTP status.

### Keepalive and idle timeout

- **Keepalive comments** (`: keepalive\n\n`, an SSE comment line clients ignore) fire every 10s of silence — Cloudflare's own idle-connection mitigation. Keepalives re-arm after **every** real upstream chunk, so any silence gap anywhere in the stream gets them (including the pre-upstream TTFB window under eager commit). The interval was 30s until 2026-08-19: clients with shorter idle timeouts (e.g. Claude Code) read a 30s silence gap as a dropped connection and retry, even though the upstream is still working.
- **120s upstream idle timeout.** Applies only **while an upstream body is being piped** — not during the acquire/refresh/TTFB wait before the first upstream byte. If no real upstream chunk (keepalive comments do not count) arrives for 120s after piping starts, the proxy emits one final stall frame, then ends the stream cleanly. Frame shape is surface-specific:
  - Anthropic surface: `event: error\ndata: {"type":"error","error":{"type":"overloaded_error","message":"upstream stalled: no data received for 120s"}}\n\n`
  - OpenAI surface: `data: {"error":{"message":"upstream stalled: no data received for 120s","type":"api_error","code":"upstream_stall"}}\n\n`

  Logged as `error_code: "upstream_stall"` unconditionally on an idle-timeout close — see [logging.md](./logging.md) for the full close-reason → `error_code` mapping.
- **First-byte (headers) timeout.** Each upstream attempt's fetch — the wait for upstream **response headers** — is bounded by `UPSTREAM_FIRST_BYTE_TIMEOUT_MS` (env var, default **180000**; applies per attempt, both surfaces, stream and non-stream). Before this the wait was unbounded: the Worker sat through the upstream edge's own ~100–125s `524` (observed 2026-08-13, five 125s hangs) and, when even that edge gave up silently, waited until the client's own timeout. On trip: abort the fetch, exclude that candidate **for this request only** (no bench — see [providers.md](./providers.md) § Penalties), and try the next candidate; none left → the normal synthesized `upstream_unavailable`. **Do not set it below ~150s**: legitimate TTFB on large-context prefill has been observed at 95–122s — the default is a backstop against black-holed connections, not a latency target.

### In-stream errors (stream: true)

Because HTTP status/headers are already `200` when streaming starts, failures discovered after commit cannot change the status line. They surface as a **single terminal SSE error frame**, then the stream ends — same pattern as codex mid-turn failures and the stall frame above.

**Every post-commit failure must end as a structured frame — never a raw stream abort.** Claude Code classifies an in-stream failure by the error event's shape and wording to decide whether to auto-retry (its gateway protocol; a pre-output failure with a well-formed transient error is retried, a malformed one is unpredictable). So internal exceptions on the dispatch path — a bench-write failure, an adapter throw, a fetch rejection before piping — are caught and translated to the matching frame below (`api_error` type when nothing more specific applies), not surfaced via `controller.error()`. A raw abort is reserved for the one case with no channel left: the failure happened mid-frame after real bytes flowed.

| Failure | OpenAI frame (approx) | Anthropic frame (approx) | `error_code` |
|---------|----------------------|--------------------------|--------------|
| No account bound | `data: {"error":{"message","type":"invalid_request_error","code":"no_upstream_account"}}` | `event: error` + `invalid_request_error` message | `no_upstream_account` |
| All accounts benched / loop exhausted | `…code":"upstream_unavailable"}` | `event: error` + `api_error` / `upstream_unavailable` | `upstream_unavailable` |
| Upstream non-2xx after failover (non-bench) | OpenAI-shaped `error` from upstream body when JSON, else generic `upstream_error` | Anthropic `event: error` with mapped type/message | `upstream_error` |
| Idle timeout (above) | stall frame | stall frame | `upstream_stall` |

Non-stream requests still return the table in **Errors** as real HTTP status codes (`400` / `503` / pass-through upstream status). Clients that need HTTP-level errors for account/pool failures should use non-stream, or treat in-stream `error` events as terminal.

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

| Situation | HTTP (non-stream) | HTTP (`stream: true`) | `code` (approx) |
|-----------|-------------------|----------------------|-----------------|
| Missing/invalid API key | 401 | 401 (pre-dispatch; never reaches eager commit) | `invalid_api_key` |
| Model string invalid | 400 | 400 (pre-dispatch) | `invalid_model` |
| No account **bound** for provider | 400 | **200** + in-stream error frame | `no_upstream_account` |
| All bound accounts benched / unavailable | 503 (+ `Retry-After` when known) | **200** + in-stream error frame (`Retry-After` is not re-applied on the already-sent SSE response) | `upstream_unavailable` |
| Upstream non-2xx, non-bench status (outside 401/402/403/429/520/522/524) | pass through status when possible | **200** + in-stream error frame | `upstream_error` |
| Reasoning rejected | 400 | 400 (pre-dispatch / adapter before stream body) | `invalid_reasoning` |
| Degenerate tool-call loop (conversion path only — see above) | 400 | 400 (pre-dispatch) | `loop_detected` |
| Responses request needing server-side state or an unconvertible part (`previous_response_id`, `conversation`, `background`, `item_reference`, `input_file`, `input_audio` — see `POST /openai/v1/responses`) | 400 | 400 (pre-dispatch) | `unsupported_field` |
| Audio part sent to a provider with no audio wire, or an unrecognized audio `format` (see "Audio input") | 400 | 400 (pre-dispatch) | `unsupported_modality` / `unsupported_audio_format` |
| Key's spend limit reached ([pricing.md](./pricing.md)) | 429 | 429 (pre-dispatch) | `spend_limit_exceeded` (Anthropic surface: `rate_limit_error` type) |
| Upstream idle / client abort mid-stream | n/a (stream only) | 200 + stall frame or clean cancel | `upstream_stall` / `client_abort` |

Auth failures (missing/invalid API key) are envelope-shaped per **surface**, matched on request path prefix rather than on provider: `/anthropic/*` gets the Anthropic shape `{"type":"error","error":{"type":"authentication_error","message":"Missing API key"|"Invalid API key"}}`; every other path (including `/openai/*`) keeps the OpenAI shape shown in the table above.

`400 no_upstream_account` (non-stream) / in-stream `no_upstream_account` is reserved for the *unbound* case: the user has **zero** accounts for the resolved provider, so retrying can never help. When accounts exist but none is usable *right now* (every one benched — e.g. the pool's single account just got benched for its cooldown), non-stream returns `503 upstream_unavailable` with `Retry-After` set to the seconds until the earliest bench expiry (min 1, **capped at 60**) when known — a transient error agent clients retry instead of treating as fatal. The cap exists because well-behaved clients may honor `Retry-After` without an upper bound (the Anthropic SDK sleeps for whatever the header says): when the earliest recovery is a weekly usage reset days out, telling the client "come back in 3 days" turns one exhausted account into a client-side outage — a retry against a 503 costs microseconds, so err on retrying too soon. On `stream: true` the same condition is an in-stream error (HTTP already `200`). If the failover loop itself exhausts the candidate list mid-request (every candidate it reached failed with a bench-type status), the result is the **same synthesized `503 upstream_unavailable` + `Retry-After`**, recomputed from the just-updated bench/limit facts across the tried candidates — never the last upstream response passed through verbatim. Passing it through (the pre-v3.5.0 behavior) handed the client one account's raw `429` whose `Retry-After` can point at that account's weekly reset days out — a gateway-aware client honoring that header then benches this whole proxy for one account's reset (measured 2026-08-14: a downstream failover client cooled the proxy off for 2.3 days over exactly this) — or, for codex, an HTML edge-challenge wall. Stream mode emits the same condition as the in-stream `upstream_unavailable` frame. Non-bench upstream errors are unchanged: the first non-bench response still returns/pipes through immediately (`upstream_error`).

**`x-should-retry` marker.** Claude Code's gateway protocol reads this response header to override its retry classification. The proxy sets `x-should-retry: false` on failures where a retry can never help — `400 no_upstream_account`, `400 invalid_model`, `400 loop_detected`, `400 unsupported_modality`, `400 unsupported_field`, `429 spend_limit_exceeded` — so the client doesn't burn its ~10-attempt retry budget on them, and `x-should-retry: true` on the synthesized `503 upstream_unavailable` (transient by construction). Other statuses carry no marker and keep the client's default classification.

Authenticated pre-dispatch failures (invalid model, no upstream account, loop-guard trip) are all logged as one `request_logs` row via `waitUntil`, same as a real dispatch; unauthenticated 401s are never logged — see [logging.md](./logging.md).

## Rate limits

No platform per-request rate limit. Upstream rate limits apply; the pool benches on 401/402/403/429 (300s, or for 429 until the upstream reset when derivable) and briefly (30s) on upstream edge-timeout statuses 520/522/524; a `529` is never benched but gets one same-account retry before any bytes are piped; an account whose usage snapshot shows an exhausted window is skipped until that window resets ([providers.md](./providers.md) § Routing module). A key with a configured **spend limit** gets 429 `spend_limit_exceeded` once its window's estimated spend reaches the ceiling — see [pricing.md](./pricing.md). The check is pre-dispatch and never counts a 429'd request itself as spend.

## Changelog (admin)

### `GET /api/changelog`

Session-auth JSON for the admin UI: the running Worker version, the newest published release, an update flag, and the sanitized release list — sourced from this repo's GitHub Releases, cached in KV. `?refresh=true` bypasses the freshness window. Full contract (response shape, caching, stale-serve, sanitization) in [changelog.md](./changelog.md).
