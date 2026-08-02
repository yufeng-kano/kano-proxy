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
  - `reasoning`: `{ effort, summary: "auto" }` built from the client effort; omitted when unset or `none`; efforts above `xhigh` clamp to `xhigh` (codex Responses models top out at `xhigh`; `max` is not a codex value — see [api.md](./api.md)).
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
- `reasoning_effort` passthrough with ceiling clamp (OpenAI body; Anthropic optional extension if present): efforts above `xhigh` are lowered to `xhigh` — grok's provider-wide top (docs.x.ai lists `high` for grok-4.5, `xhigh` for grok-4.20-multi-agent; live grok-4.5 accepts `xhigh`, and `max` is not an xAI value). See [api.md](./api.md).
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

## Custom endpoints (user-defined)

A custom endpoint is a user-defined *provider*: a slug, a base URL, an API key, and a wire **format** (`openai` | `anthropic`). Once created it behaves like a built-in provider on both surfaces — same model-id shape (`slug/upstream`), same account pool / bench / failover — but it is never added to the static builtin registry (`providers/index.ts`); its adapter (`providers/custom_openai.ts` / `providers/custom_anthropic.ts`) is instantiated per-request from the `custom_providers` D1 row via `providers/resolve.ts`.

- **Data model:** the provider-level config (`slug`, `name`, `format`, `base_url`, `models_mode`, manual model list) lives in `custom_providers` (see [database.md](./database.md)). Its API key(s) live as ordinary rows in `upstream_accounts` with `provider = slug` — the existing pool/bench/promote/remove machinery is inherited unchanged, nothing is special-cased for a custom slug. The data model supports **multiple keys per custom provider** (same multi-account pool as built-ins), but the admin REST routes in this MVP create exactly **one key at create time** and **replace it in place** on update — see [auth.md](./auth.md).
- **Slug rules:** lowercase, 2–32 chars, `[a-z0-9]` plus internal hyphens, must start and end alphanumeric (`^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$`). Unique per user. **Immutable after creation** (rename `name` instead). Reserved (rejected): `claude-code`, `codex`, `grok`, `openai`, `anthropic`, `claude`, `gpt`, `gemini`, `google`, `api`, `admin`, `custom`, `models`, `usage`, `keys`, `accounts`, `kano`, `kano-proxy`. `format` is also **immutable after creation** — changing wire format is a delete-and-recreate.
- **Limits:** max **20** custom providers per user; `api_key` 1–512 chars; `name` 1–64 chars; `base_url` ≤ 300 chars; manual model list ≤ 100 entries, each trimmed to 1–128 chars with no whitespace (`/` is allowed — upstream ids may be namespaced, e.g. an OpenRouter-style `org/model`).
- **Base URL validation (`utils/upstream_url.ts`), applied on create, update, and the test-connection endpoint:** must parse as a URL; scheme must be `https`; no embedded username/password; no query string; no fragment; trailing slash(es) stripped on save. Hostname must not be: this deploy's own host (compared against both the admin request's `Host` header and the hostname of `APP_URL`, when set), `localhost` / `*.localhost` / `*.local`, an IPv4 literal in `127.0.0.0/8`, `0.0.0.0/8`, `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `169.254.0.0/16`, or an IPv6 literal that is loopback (`::1`), link-local (`fe80::/10`), or unique-local (`fc00::/7`). This is a loop/SSRF guard, not a network allowlist — a public hostname that merely *resolves* to a private IP at request time is not caught (DNS is not re-checked per proxied request).
- **Endpoint construction is literal concatenation — no magic `/v1` insertion:**
  - `format=openai`: `${base_url}/chat/completions`, `${base_url}/models` (the user includes `/v1` in `base_url` themselves, per the OpenAI-SDK convention of a base that already ends in `/v1`).
  - `format=anthropic`: `${base_url}/v1/messages`, `${base_url}/v1/messages/count_tokens`, `${base_url}/v1/models` (the user's `base_url` has **no** `/v1`, per the Anthropic-SDK convention).
- **Auth headers:**
  - `openai`: `Authorization: Bearer <key>`.
  - `anthropic`: `x-api-key: <key>`. `anthropic-version`: on the native `/anthropic` surface, forwards the client's header verbatim when present, else defaults to `2023-06-01`; the `/openai/v1` conversion path always sends the hardcoded default (no client header exists to forward on that surface). `anthropic-beta` is forwarded **verbatim** from the client on the native surface (or omitted entirely if absent) — **never** the Claude-Code OAuth base betas, **never** the auto-added `effort-2025-11-24` beta, **never** the fixed Claude-Code system-line prepend. All three of those are OAuth-compat behaviors specific to the `claude-code` adapter; a custom Anthropic-compatible endpoint gets a plain passthrough.
- **`/openai/v1` (`custom-openai` adapter, format=openai):** `chatCompletions()` is a near-passthrough — rewrite `model` to the bare upstream id, forward the rest of the client's body **verbatim** (including `temperature`, `reasoning_effort`, `response_format`, and any other field the built-in adapters don't model), pipe SSE straight through without buffering. This deliberately diverges from every built-in adapter, which strip `temperature` and clamp `reasoning_effort` to a provider ceiling — a custom endpoint has neither. No `messages()` / `countTokens()` — the `/anthropic` surface reaches this adapter through the existing Anthropic→OpenAI conversion path (`dispatchAnthropicViaOpenAI`), exactly like grok/codex (`cache_control` stripped there, same as any other conversion). `listModels()` is `GET {base}/models`.
- **`/anthropic` (`custom-anthropic` adapter, format=anthropic):** `messages()` / `countTokens()` are a native passthrough (mirrors the claude-code adapter's `forwardToAnthropic` minus every OAuth specific listed above) — the body (including `cache_control` and `thinking`) goes through untouched aside from the `model` rewrite the route already does for every native-passthrough provider. `chatCompletions()` (the `/openai/v1` surface) builds an Anthropic Messages request via the same shared converters `openaiToAnthropicMessages` / `anthropicToOpenAIResponse` the claude-code adapter uses — **`reasoning_effort` is dropped on this surface** (no `thinking`/`output_config` synthesized from it); the native `/anthropic` surface is where a custom-anthropic endpoint gets full `thinking` control, by sending it directly in the request body. `listModels()` is `GET {base}/v1/models`.
- **Neither adapter** implements `fetchUsage` or `refreshIfNeeded` — a custom provider's key is a static secret with no OAuth refresh flow and no usage/billing surface to poll.
- **`count_tokens` (`POST /anthropic/v1/messages/count_tokens`):** a custom **anthropic**-format provider forwards through the same pool/failover loop as `claude-code` (`countTokens()` exists on its adapter). A custom **openai**-format provider gets the exact same `400` rejection grok/codex get — there is no Chat Completions token-counting endpoint to convert to.
- **Reasoning ceilings (`utils/reasoning.ts`):** the builtin `CEILING` map stays typed to the builtin `ProviderId` union and is never consulted for a custom provider — custom-openai forwards `reasoning_effort` unclamped, custom-anthropic drops it on the `/openai/v1` surface as described above. The client-supplied value is still validated against the reasoning ladder (`parseReasoningEffort` — `400` on garbage) before either adapter ever sees it; only the provider-specific ceiling clamp is skipped.
- **Catalog (`catalog/models.ts`):** after the builtin loop, one section per custom provider is appended. `models_mode=manual` returns the stored list directly (never fetches). `models_mode=auto` tries the adapter's `listModels()` with an acquired key, using the **same 90s KV cache** as built-ins (cache key scoped `user+slug`, reusing the identical cache helpers); on failure — or when there is no usable key to query — it falls back to the stored manual list when non-empty, else an empty list. **Never fabricates a catalog.** Ids are always `slug/<upstream_id>`.
- **Admin REST:** `/api/custom-providers` — see [auth.md](./auth.md) for the route table and [admin-ui.md](./admin-ui.md) for the UI.

## Failover order

Within one user’s provider pool only. Never cross users. Applies identically to custom providers (pooled under their slug).

## Catalog

- **Claude Code / Grok:** live upstream model lists when an account is bound.
- **Codex:** empty list (no upstream or third-party list API); see Codex section.
- **Custom providers:** manual list, or live + 90s cache with manual/empty fallback — see the Custom endpoints section above.
- `GET /openai/v1/models` and `GET /anthropic/v1/models` share the same catalog; ids are always `provider/upstream`.
- Only providers the user can use are queried. Unknown model strings may still be attempted if that provider is bound.
