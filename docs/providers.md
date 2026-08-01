# Providers and pools

## Common pool interface

Per `(user_id, provider)`:

1. **List** accounts with priority and metadata  
2. **Acquire** highest-priority non-benched credential (refresh if needed)  
3. **Bench** on upstream `401` / `403` / `429` for ~300s (KV)  
4. **Promote** / **Remove**  
5. **Usage snapshot** — KV **90s** per account; `?refresh=true` bypasses. Frontend sessionStorage 90s + poll 90s.

Timeouts: do not permanently shrink the pool on transport timeout alone when avoidable; prefer per-request exclude + retry.

## Claude Code

- Upstream: Anthropic Messages + OAuth usage/profile endpoints (see lincy-agent `docs/dev/provider-api-spec.md`).
- Required system line for OAuth CLI-compat (prepend if absent, fixed string).
- Usage: 5h, 7d, scoped weekly.
- **Surfaces:** both `/openai/v1` and `/anthropic` with model `claude-code/<upstream_id>`.
- OpenAI surface: Chat Completions ↔ Messages (stream, tools, vision, json_schema).
- Anthropic surface: body passthrough + auth inject + beta merge (native; no format convert).
- **`count_tokens` passthrough:** `POST /anthropic/v1/messages/count_tokens` forwards to upstream `/v1/messages/count_tokens` using the exact same auth/header construction and account pool/failover loop as `/v1/messages` (see [api.md](./api.md)). `grok`/`codex` have no equivalent endpoint to convert a token count to, so that route rejects those models with `400` instead of attempting a conversion.
- **`effort-2025-11-24` beta:** the native `/anthropic` path (`messages()`) adds `effort-2025-11-24` to the outgoing `anthropic-beta` header automatically whenever the (patched) request body contains an `output_config` key — a client sending `output_config: {"effort": ...}` without its own effort beta header would otherwise get no effort beta upstream, and Anthropic rejects/ignores `output_config` in that case. Deduped the same way client-supplied extra betas are: a client that already lists `effort-2025-11-24` is not double-added. The 4 base betas (`oauth-2025-04-20`, `claude-code-20250219`, `interleaved-thinking-2025-05-14`, `fine-grained-tool-streaming-2025-05-14`) are unaffected. The `/openai/v1` path (`chatCompletions()`, which builds its own Anthropic Messages body via `mapReasoning`) always sends `effort-2025-11-24` unconditionally rather than checking for `output_config`, since that adapter method is the one place that constructs it.
- **Caching is explicit, unlike Grok/Codex.** Anthropic only caches at `cache_control` breakpoints (max 4/request; min cacheable prefix 512–4096 tokens by model; 5m default / 1h TTL; cache writes cost 1.25×/2×, reads ~0.1×). There is no conv-id or sticky-routing header — placement is the whole mechanism.
  - `/anthropic` → claude-code: clients that set their own breakpoints (Claude Code does) keep them through our passthrough, so caching already works end to end. Do not touch them.
  - `/openai/v1` → claude-code: we never add `cache_control`, so **these requests never cache**. That is deliberate, not an oversight — a breakpoint on a one-off request costs 1.25× and reads back nothing. Adding one only pays off when the same prefix repeats, which the proxy cannot know from a single stateless request. Revisit only with a measured repeat-rate, and document the placement rule here first.

## Codex

- Upstream: ChatGPT `codex/responses` (reverse-engineered; headers from lincy).
- **Surfaces:** both `/openai/v1` and `/anthropic` with model `codex/<upstream_id>`.
- OpenAI surface: Chat Completions ↔ Responses SSE.
- Anthropic surface: Messages ↔ Chat Completions (strip `cache_control`) ↔ Responses SSE.
- **Request body sent upstream (`/codex/responses`):**
  - `instructions`: all `role: "system"` messages are pulled out of `messages`/`input`, their text joined in order with `"\n\n"`, and sent as this top-level Responses field — they are never sent as fake `role: "user"` items inside `input`. This applies on both surfaces, since the Anthropic `system` field is first converted into an OpenAI `system` message before reaching the codex adapter.
  - `store: false`: sent unconditionally on every request (the Responses API default is `store: true`, which this proxy does not want).
  - `max_output_tokens`: **never sent** — the backend 400s on it for every verified OAuth model (see [api.md](./api.md)).
  - `tool_choice`: mapped from the client's OpenAI `tool_choice` to the Responses flattened shape (`"auto"`/`"none"`/`"required"` pass through; `{type:"function", function:{name}}` → `{type:"function", name}`); defaults to `"auto"` when `tools` are present but the client sent no `tool_choice`; omitted entirely when there are no `tools` (upstream rejects `tool_choice` without `tools`).
  - `prompt_cache_key`: forwarded verbatim when the client sends a non-empty string (official OpenAI Chat Completions field). Pairs with deterministic account acquisition (`ORDER BY priority DESC, created_at DESC`, first non-benched account wins — see `db/accounts.ts` / `pool/acquire.ts`) so the same upstream ChatGPT account usually serves every turn, and a stable key produces real upstream prompt-cache hits.
- **Reasoning summary → `reasoning_content`:** `response.reasoning_summary_text.delta` events are surfaced using the de-facto `reasoning_content` extension field (DeepSeek/OpenRouter convention): `delta.reasoning_content` on Chat Completions stream chunks, `message.reasoning_content` on the non-stream completion. On the `/anthropic` surface these are dropped harmlessly by the OpenAI→Anthropic converter (it only reads `content` / `tool_calls`) rather than mapped to an Anthropic `thinking` block.
- **Upstream failure events (`response.failed` / `error`):** treated as a hard failure, never a fabricated success. Streaming (`codexSseToOpenAIStream`): a single OpenAI-shaped error line replaces the rest of the turn and the stream ends there — no `finish_reason` chunk, no `[DONE]`. Non-stream (`collectCodexSse`): the adapter returns `502` with `{"error":{"message","type":"upstream_error"}}` instead of a `200` completion built from a partial/empty turn. On the `/anthropic` surface (`openaiSseToAnthropicStream`), the same failure becomes an Anthropic `event: error` and the stream ends there too — see [api.md](./api.md).
- **Models:** ChatGPT OAuth has **no** public `/models` endpoint. Platform `GET api.openai.com/v1/models` rejects these tokens (`api.model.read` missing). There is also **no trusted third-party API** that returns per-account Codex OAuth inventory. kano-proxy returns an **empty** list (no hard-coded catalog). Admin UI links official docs: [OpenAI models](https://developers.openai.com/api/docs/models), [ChatGPT / Codex models](https://learn.chatgpt.com/docs/models). Clients may still send a model id if a Codex account is bound; unknown/unsupported ids fail at upstream.
- Usage: dynamic windows (label 5h / Week / Nd) from `/codex/usage` (alias `/wham/usage`).
- Usage fetch: CLI `User-Agent: codex_cli_rs/0.144.3`. chatgpt.com edge **403 bot-challenges by TLS/client fingerprint, not headers** (verified 2026-08-01: same headers/IP → stdlib urllib 401 JSON passes the wall, curl and workerd `fetch` get 403 HTML). lincy passes only because Python urllib's fingerprint is allowed; a Worker cannot change its `fetch` fingerprint, so header tuning cannot fix this. When blocked, account stays **active/standby** (not unusable); UI omits usage bars (chat still works).

## Grok

- Upstream: `api.x.ai` OpenAI-compatible Chat Completions with SuperGrok OAuth.
- Multi-account pool (product requirement; not limited to lincy’s single file).
- **Surfaces:** both `/openai/v1` and `/anthropic` with model `grok/<upstream_id>`.
- OpenAI surface: near-passthrough Chat Completions.
- Anthropic surface: Messages ↔ Chat Completions (strip `cache_control`; no invented sticky ids).
- `reasoning_effort` passthrough (OpenAI body; Anthropic optional extension if present).
- Client identity: present as the official CLI (`User-Agent: grok-shell/<ver> (linux; x86_64)`, `x-grok-client-identifier: grok-shell`), same rationale as codex's `originator: codex_cli_rs` — the subscription OAuth surface is the CLI's, and providers gate on client shape (cf. chatgpt.com bot wall). Shape/version from `xai-org/grok-build` (`xai-grok-sampler/src/client.rs`, `xai-grok-version`). **Not** a billing lever: nothing in that source ties client identity to metering.
- Sticky header `x-grok-conv-id` forwarded when the client supplies it on **either** surface (also `x-grok-session-id`, `x-grok-turn-idx`); `x-grok-req-id` generated per request. **Never synthesize a conv id** — see the measurement below.
- **Measured 2026-08-01**, 4-turn conversation over a 2016-token shared prefix, each arm with its own fresh system prompt, order counterbalanced:

  | conv-id strategy | turn 0 | turn 1 | turn 2 | turn 3 |
  |---|---|---|---|---|
  | none (absent) | 8% | 98% | 97% | 99% |
  | client-supplied, stable | 8% | 97% | 98% | 98% |
  | hash of full message history (changes per turn) | 10% | 9% | 99% | 9% |

  xAI's prefix cache already works **without** conv-id, so forwarding a stable id is neutral-to-positive and auto-deriving one buys nothing. A *changing* id is actively destructive — it re-routes each turn and thrashes the cache. Hence: forward what the client sends, never invent one.
  (An earlier run appeared to show conv-id lifting hits 10%→98%; that arm was confounded — both arms shared one system prompt, so arm 1 warmed the cache for arm 2.)
- Usage: `cli-chat-proxy.grok.com` billing (`creditUsagePercent`) when auth allows; mark stale on failure.

## Failover order

Within one user’s provider pool only. Never cross users.

## Catalog

- **Claude Code / Grok:** live upstream model lists when an account is bound.
- **Codex:** empty list (no upstream or third-party list API); see Codex section.
- `GET /openai/v1/models` and `GET /anthropic/v1/models` share the same catalog; ids are always `provider/upstream`.
- Only providers the user can use are queried. Unknown model strings may still be attempted if that provider is bound.
