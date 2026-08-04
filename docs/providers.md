# Providers and pools

## Common pool interface

Per `(user_id, provider)`:

1. **List** accounts with priority and metadata  
2. **Acquire** highest-priority non-benched credential (refresh if needed)  
3. **Bench** on upstream `401` / `403` / `429` for ~300s (KV)  
4. **Promote** / **Remove**  
5. **Usage snapshot** — cached **60s server-side in D1** on the `upstream_accounts` row, behind a single-flight lock. See "Usage cache" below.

Timeouts: do not permanently shrink the pool on transport timeout alone when avoidable; prefer per-request exclude + retry.

## Usage cache (server-side, 60s, D1)

`GET /api/providers/{provider}/accounts` used to call `fetchUsage` live for every account on every request. The only protection was the admin UI's 90s localStorage TTL + 90s poll — which is **per device**, so N signed-in devices meant N× upstream calls, against endpoints that rate-limit and bot-wall. Two devices was enough to feel it.

The snapshot now lives on the account row (`usage_snapshot_json`, `usage_fetched_at`, `usage_fetching_at` — see [database.md](./database.md)), shared by every device and tab:

- **Fresh** (`usage_fetched_at` within **60s**): return the stored snapshot, no upstream call. This is the only case that skips the network.
- **Stale / missing / `?refresh=true`**: fetch upstream **synchronously** and return the fresh result. Not stale-while-revalidate — with a 90s frontend poll against a 60s TTL, *every* steady-state poll takes the stale path, so revalidate-in-background would render one cycle behind forever and a newly added account would show no usage at all until the second poll.
- Net effect: upstream calls are capped at **1 per account per 60s** regardless of device count, and single-device freshness is unchanged from the live-fetch behavior it replaces.

**Single-flight lock.** D1 has no cross-request transactions, so the lock is a conditional `UPDATE` used as a compare-and-swap — SQLite's single-statement atomicity plus `meta.changes`:

```sql
UPDATE upstream_accounts SET usage_fetching_at = ?now
 WHERE id = ? AND (usage_fetching_at IS NULL OR usage_fetching_at < ?now_minus_30s)
```

`changes === 1` wins and calls upstream; `changes === 0` means someone else is already fetching, and that caller returns the stored snapshot rather than queueing. The `< now-30s` clause breaks a lock orphaned by a dead Worker or a hung upstream.

**Release must be compare-and-release** (`WHERE id = ? AND usage_fetching_at = ?mine`), folded into the same statement that writes the snapshot. An unconditional release is a real bug, not a style point: if A hangs, the breaker lets B acquire at t+31s, and A's late unconditional release would clear *B's* lock and let C start a third concurrent fetch — exactly what the lock exists to prevent.

**On upstream failure, never overwrite a good snapshot.** Release the lock and let the stored windows stand, surfacing the error alongside them; one upstream hiccup must not blank the bars. The snapshot therefore stores `error`, `stale` and `edgeBlocked` too — `status: "unusable"` is derived from all three (`routes/providers.ts`), so a cache hit that dropped them would silently flip an unusable account back to active.

**Why D1 and not KV.** KV's minimum `cacheTtl` is 60s and writes propagate with eventual consistency, so a 60s refresh cadence would surface data up to ~2 minutes old — the cache would defeat the freshness it exists to protect. More decisively, **KV cannot express a compare-and-swap**, so the single-flight lock needs D1 regardless; once D1 is in the design, a second store buys nothing. (An earlier revision justified having no usage cache with "KV free-tier writes are the scarce resource". That reasoning was already stale for the reference deployment — `[limits] cpu_ms` in `wrangler.toml` requires Workers Paid, where the budget is 1M KV writes/month, not 1k/day.)

**Why not a cron trigger.** Polling upstream on a schedule would run 24/7 whether or not anyone is looking, multiplying upstream calls against the same rate limits this cache exists to respect. The demand-driven model — nobody watching, nothing fetched — is the cheaper one; the frontend reinforces it by stopping its poll whenever the page is hidden ([admin-ui.md](./admin-ui.md)).

Two providers make the saving concrete: grok's `fetchUsage` is two upstream calls plus a possible token refresh, and codex usage egresses through the paid Cloud Run relay ([codex-relay.md](./codex-relay.md)).

## Claude Code

- Upstream: Anthropic Messages + OAuth usage/profile endpoints.
- Required system line for OAuth CLI-compat (prepend if absent, fixed string).
- **Client fingerprint headers.** Every upstream call (messages, `count_tokens`, `/v1/models`, usage/profile) carries the Claude Code CLI identity — `User-Agent: claude-cli/<ver> (external, cli)` plus `x-stainless-package-version` / `-runtime-version` / `-os` / `-arch`. These tokens belong to the CLI, so requests should look like it, and sending workerd's default `User-Agent` advertises a client the upstream has no reason to expect. (This is parity with the real CLI, not a bot-wall workaround — the codex block is a Cloudflare-Worker header rule that no `User-Agent` can dodge; see "The chatgpt.com wall".) On the native `/anthropic` passthrough each field is taken from the client's own header when it sent one (a genuine Claude Code client keeps its identity end to end) and falls back to the pinned baseline otherwise. The `/openai/v1` conversion surface has no client headers to relay, so it always sends the baseline.
- Usage: 5h, 7d, scoped weekly. **Two field names for the same number:** the top-level `five_hour` / `seven_day` objects carry `utilization`, but each entry of `limits[]` (where the scoped weekly rows live, labeled from `scope.model.display_name`) carries `percent`. Reading `utilization` off a `limits[]` entry yields `undefined` and renders the row as `—` with its label and reset time still correct — the shape that hid this until v1.15.1.
- **Surfaces:** both `/openai/v1` and `/anthropic` with model `claude-code/<upstream_id>`.
- OpenAI surface: Chat Completions ↔ Messages (stream, tools, vision, json_schema).
- Anthropic surface: body passthrough + auth inject + beta merge (native; no format convert).
- **`count_tokens` passthrough:** `POST /anthropic/v1/messages/count_tokens` forwards to upstream `/v1/messages/count_tokens` using the exact same auth/header construction and account pool/failover loop as `/v1/messages` (see [api.md](./api.md)). `grok`/`codex` have no equivalent endpoint to convert a token count to, so that route rejects those models with `400` instead of attempting a conversion.
- **`effort-2025-11-24` beta:** the native `/anthropic` path (`messages()`) adds `effort-2025-11-24` to the outgoing `anthropic-beta` header automatically whenever the (patched) request body contains an `output_config` key — a client sending `output_config: {"effort": ...}` without its own effort beta header would otherwise get no effort beta upstream, and Anthropic rejects/ignores `output_config` in that case. Deduped the same way client-supplied extra betas are: a client that already lists `effort-2025-11-24` is not double-added.
- **Base betas (faithful passthrough):** the native `/anthropic` path sends exactly **two** fixed betas — `oauth-2025-04-20` and `claude-code-20250219`, the ones the Claude Code OAuth upstream requires to accept the request at all — plus the client's own `anthropic-beta` list **verbatim** (deduped, client order preserved after the fixed pair). Feature betas such as `interleaved-thinking-2025-05-14` / `fine-grained-tool-streaming-2025-05-14` are **never force-added**: whether they're on is the client's choice, so proxied model behavior matches a direct Anthropic connection. (Before 2026-08-03 the proxy force-added those two feature betas to every native request; removed for faithfulness.) The `/openai/v1` path (`chatCompletions()`, which builds its own Anthropic Messages body via `mapReasoning` and has no client beta header to forward) is unchanged: it opts into `interleaved-thinking-2025-05-14`, `fine-grained-tool-streaming-2025-05-14`, and `effort-2025-11-24` unconditionally, since there the proxy authors the upstream request itself.
- **Caching is explicit, unlike Grok/Codex.** Anthropic only caches at `cache_control` breakpoints (max 4/request; min cacheable prefix 512–4096 tokens by model; 5m default / 1h TTL; cache writes cost 1.25×/2×, reads ~0.1×). There is no conv-id or sticky-routing header — placement is the whole mechanism.
  - `/anthropic` → claude-code: clients that set their own breakpoints (Claude Code does) keep them through our passthrough, so caching already works end to end. Do not touch them.
  - `/openai/v1` → claude-code: we never add `cache_control`, so **these requests never cache**. That is deliberate, not an oversight — a breakpoint on a one-off request costs 1.25× and reads back nothing. Adding one only pays off when the same prefix repeats, which the proxy cannot know from a single stateless request. Revisit only with a measured repeat-rate, and document the placement rule here first.

## Codex

- Upstream: ChatGPT `codex/responses` (reverse-engineered; CLI-compatible headers).
- **Surfaces:** both `/openai/v1` and `/anthropic` with model `codex/<upstream_id>`.
- OpenAI surface: Chat Completions ↔ Responses SSE.
- Anthropic surface: Messages ↔ Chat Completions (strip `cache_control`) ↔ Responses SSE.
- **Upstream request headers (`/codex/responses`)** — aligned with the Codex CLI. (Correctness/parity, not a bot-wall workaround: these headers do not affect whether the edge blocks us — see "The chatgpt.com wall" below.)
  - `User-Agent: codex-tui/<ver> (<os>; <arch>) <term>/<ver>` and `Originator: codex-tui`. Sending no `User-Agent` at all (workerd's default) is a bot-wall risk; the CLI value is what the upstream expects.
  - `Connection: Keep-Alive`, `Accept: text/event-stream`, `chatgpt-account-id`, `session_id`.
  - **`OpenAI-Beta` is not sent.** The reference CLI proxy never sets it on the REST Responses call (only on the WebSocket path); it is not required to make `/codex/responses` accept a request.
- **Request body sent upstream (`/codex/responses`):**
  - `instructions`: all `role: "system"` messages are pulled out of `messages`/`input`, their text joined in order with `"\n\n"`, and sent as this top-level Responses field — they are never sent as fake `role: "user"` items inside `input`. This applies on both surfaces, since the Anthropic `system` field is first converted into an OpenAI `system` message before reaching the codex adapter. Any `role: "system"` item that still reaches `input` is rewritten to `role: "developer"` (the Responses API's own name for it).
  - `store: false`: sent unconditionally on every request (the Responses API default is `store: true`, which this proxy does not want).
  - `include: ["reasoning.encrypted_content"]`: sent unconditionally. Without it the upstream never returns reasoning items, so there is nothing to replay on the next turn and every turn of a multi-turn agent re-reasons from scratch.
  - `parallel_tool_calls`: `true` when the request carries `tools`; dropped entirely when it does not (upstream rejects the field without tools).
  - **Fields stripped before the upstream call** (the backend rejects them): `max_output_tokens`, `max_completion_tokens`, `temperature`, `top_p`, `truncation`, `user`, `previous_response_id`, `generate`, `prompt_cache_retention`, `safety_identifier`, `stream_options`, and `service_tier` unless its value is exactly `"priority"`.
  - **`call_id` length:** Responses `call_id`s longer than 64 chars are shortened to a 64-char prefix plus a `_<sha256-prefix>` suffix, applied consistently to `function_call` and its matching `function_call_output` so the pair still lines up. Claude Code emits tool ids long enough to hit this.
  - `max_output_tokens`: **never sent** — the backend 400s on it for every verified OAuth model (see [api.md](./api.md)).
  - `tool_choice`: mapped from the client's OpenAI `tool_choice` to the Responses flattened shape (`"auto"`/`"none"`/`"required"` pass through; `{type:"function", function:{name}}` → `{type:"function", name}`); defaults to `"auto"` when `tools` are present but the client sent no `tool_choice`; omitted entirely when there are no `tools` (upstream rejects `tool_choice` without `tools`).
  - `reasoning`: `{ effort, summary: "auto" }` built from the client effort; omitted when unset or `none`; efforts above `xhigh` clamp to `xhigh` (codex Responses models top out at `xhigh`; `max` is not a codex value — see [api.md](./api.md)).
  - `prompt_cache_key`: forwarded verbatim when the client sends a non-empty string (official OpenAI Chat Completions field). Pairs with deterministic account acquisition (`ORDER BY priority DESC, created_at DESC`, first non-benched account wins — see `db/accounts.ts` / `pool/acquire.ts`) so the same upstream ChatGPT account usually serves every turn, and a stable key produces real upstream prompt-cache hits.
- **Reasoning summary → `reasoning_content`:** `response.reasoning_summary_text.delta` events are surfaced using the de-facto `reasoning_content` extension field (DeepSeek/OpenRouter convention): `delta.reasoning_content` on Chat Completions stream chunks, `message.reasoning_content` on the non-stream completion. On the `/anthropic` surface these are dropped harmlessly by the OpenAI→Anthropic converter (it only reads `content` / `tool_calls`) rather than mapped to an Anthropic `thinking` block.
- **Upstream failure events (`response.failed` / `error`):** treated as a hard failure, never a fabricated success. Streaming (`codexSseToOpenAIStream`): a single OpenAI-shaped error line replaces the rest of the turn and the stream ends there — no `finish_reason` chunk, no `[DONE]`. Non-stream (`collectCodexSse`): the adapter returns `502` with `{"error":{"message","type":"upstream_error"}}` instead of a `200` completion built from a partial/empty turn. On the `/anthropic` surface (`openaiSseToAnthropicStream`), the same failure becomes an Anthropic `event: error` and the stream ends there too — see [api.md](./api.md).
- **Reasoning replay cache (both surfaces).** Codex returns reasoning items carrying `encrypted_content`; the Responses API expects them echoed back in `input` on the next turn of the same conversation. Claude Code and the OpenAI Chat Completions wire format both have nowhere to carry an opaque Responses reasoning item, so the client cannot echo it and the proxy must. After a **completed** turn, the reasoning items (plus the turn's `function_call` items, which must stay adjacent to them) are stored in KV (`CACHE`, TTL 1h) keyed by SHA-256(`api_key_id` + upstream model + session id), mirroring the grok replay cache. On the next turn of the same session+model, they are re-injected into Responses `input` ahead of the new user message. Session id comes from the client affinity headers; **no session id ⇒ the cache is a no-op** (single-turn requests are unaffected). Writes are scheduled with `waitUntil` so they survive Response close. KV is eventually consistent — under load a put may not be visible on the immediate next turn. A completed turn with no replayable reasoning **clears** the prior entry. Replay is also gated on the previous assistant text still hashing to what produced the items, so an edited history simply skips the injection. Cached items are merged rather than blindly prepended: a `function_call` the client already echoed (Claude Code does) is skipped instead of duplicated, cached reasoning is skipped when the input already carries its own, and the items are spliced in ahead of the first tool result — the point where the prior assistant turn ended. Unlike grok, there is **no** strip-and-retry recovery on an invalid-signature `400` yet; such a turn fails to the client and the entry is replaced on the next completed turn.
- **Models:** served from `GET https://chatgpt.com/backend-api/codex/models?client_version=<ver>` with the CLI headers above, returning `{"models":[{"slug","display_name",…}]}` → ids `codex/<slug>`. Entries whose `visibility` is `hide` are omitted. **Not** the Platform endpoint: `GET api.openai.com/v1/models` rejects ChatGPT OAuth tokens (`api.model.read` missing). Requests go through the egress relay when configured (see "The chatgpt.com wall" below and [codex-relay.md](./codex-relay.md)); on `403`/HTML (relay unset) or any other failure, the list falls back to the public catalog mirror (`models.router-for.me` / the `router-for-me/models` GitHub raw JSON), which is the same list the Codex CLI proxy ships. Both paths share the standard 1h catalog cache. If every source fails the list is **empty** — never a hard-coded or invented catalog. Clients may still send a model id if a Codex account is bound; unknown ids fail at upstream.
- Usage: dynamic windows (label 5h / Week / Nd) from `/codex/usage` (alias `/wham/usage`).
- Usage fetch: CLI `User-Agent: codex_cli_rs/0.144.3`, via the egress relay when configured. When blocked (direct mode — see below), the account stays **active/standby** (not unusable); UI omits usage bars.

### The chatgpt.com wall — measured, 2026-08-03

**Every chatgpt.com endpoint this proxy uses (`/codex/responses`, `/codex/usage`, `/codex/models`) returns `403` + an HTML challenge page when called _directly_ from a Cloudflare Worker.** The block happens at OpenAI's edge, before the model, so **no tokens are billed**. Production therefore routes all chatgpt.com traffic through the approved [Cloud Run egress relay](./codex-relay.md); with `CODEX_RELAY_URL` unset, codex chat fails exactly as this section describes.

**The cause is a header rule that targets Cloudflare Workers specifically.** Cloudflare's outbound `fetch()` automatically attaches `CF-Worker`, `CF-Connecting-IP`, `CDN-Loop`, `CF-Ray` and friends to every subrequest. Isolating them one at a time from a residential IP with plain `curl` — same TLS, same everything, a deliberately fake bearer token so nothing could be billed:

| Request | Result |
|---|---|
| Baseline, no CF headers | `401` JSON ✅ |
| `+ CDN-Loop`, `+ CF-Ray`, `+ X-Forwarded-For` | `401` JSON ✅ |
| **`+ CF-Worker`** (any value, incl. empty) | **`403` HTML** ❌ |
| **`+ CF-Connecting-IP`** (any value) | **`403` HTML** ❌ |

Presence alone triggers it; the value is irrelevant. Header tuning on our side cannot help, because these two are added by the platform, not by this code.

**`/codex/usage` carries a second, independent block.** Same probe, same residential IP, no CF headers at all: `/codex/responses` and `/codex/models` pass (405 / 401), but `/codex/usage` is `403` HTML for everyone — while its alias **`/wham/usage` passes with `401` JSON** (stable across repeated runs). So usage has a path-level rule of its own, on top of the Worker rule. This does not help us yet: `/wham/usage` still 403s once `CF-Worker` is attached, so from a Worker both aliases are blocked. It does mean that behind a non-Worker egress, `/wham/usage` is the alias to prefer — `codex_usage.ts` already tries it.

**Two earlier claims in this doc were wrong — do not reintroduce them:**

- *"The edge fingerprints TLS/JA3, not headers."* False for this endpoint. Plain `curl` (HTTP/2 and forced HTTP/1.1) and Python `urllib` all pass the wall from a residential IP; sending `user-agent: Cloudflare-Workers` still passes. Swapping `User-Agent` / `Originator` / `Accept` changes nothing either way. TLS fingerprinting is real for *some* codex clients (see `openai/codex#17860`, where Linux rustls is flagged and macOS native-tls is not) but it is **not** what blocks this proxy.
- *"curl gets 403 too, only stdlib urllib passes."* False. Both pass. The original note conflated two different failure modes.

Consequences for the code:
- `shouldBenchStatus` treats the `403` as an auth failure and benches the account for 5 minutes; because the body is HTML, the passthrough error reaches the client as a wall of markup.
- `listModels` is unaffected in practice — it falls back to the public catalog mirrors, which are not behind this rule.
- The CLI `User-Agent` / `Originator` / `Connection` / `session_id` headers this adapter now sends are still correct (they match the reference implementation and cost nothing), but they were **not** the fix, and v1.14.0 reproduced the identical `403` with them in place.

**A Worker cannot remove these headers — do not spend time retrying this.** Cloudflare's edge proxy injects them *after* the Worker's JS returns, so a `headers.delete()` in our code runs too early to matter. It appears to work under `wrangler dev` (Miniflare even has a local-only `stripCfConnectingIp` option) and then fails in production — a trap worth knowing about. The suppression is deliberate: `CF-Worker` is documented as *"added to all Worker subrequests sent via `fetch()`"* whose purpose is to *"recognize, filter, and route traffic generated by Workers"* ([HTTP headers reference](https://developers.cloudflare.com/fundamentals/reference/http-headers/#cf-worker)), and [Transform Rules](https://developers.cloudflare.com/rules/transform/request-header-modification/) separately forbid modifying any `cf-`-prefixed request header. Every escape route checked is closed:
- **`cf: {...}` fetch options** — none of the twelve documented properties touch CF-* headers, and unknown keys are silently ignored, so a guessed flag fails without erroring.
- **`resolveOverride`, Smart Placement, Hyperdrive, Durable Objects, Outbound Workers** — these change where a request originates, not what the proxy appends.
- **`cloudflare:sockets` raw TCP** — blocked outright for this target: [outbound sockets to Cloudflare IP ranges are refused](https://developers.cloudflare.com/workers/runtime-apis/tcp-sockets/#considerations), and `chatgpt.com` resolves inside those ranges (`172.64.0.0/13`, `104.16.0.0/13`). Cloudflare has stated there are no plans to re-enable it. It would also have meant hand-rolling HTTP/1.1 + chunked parsing for SSE.

Caveat on sourcing: there is no single Cloudflare sentence stating "you cannot remove `CF-Worker`". The conclusion is assembled from the "all subrequests" wording, the `cf-` Transform Rules prohibition, the proxy-runs-last ordering, and the Workers architect's by-design statements. Convergent enough to act on, but it is inference from several sources rather than one explicit denial.

**The block is not IP-reputation based — verified, not assumed.** Running the same probe from a GitHub Actions runner (Azure datacenter IP `134.33.67.170`, i.e. exactly the kind of cloud egress that "datacenter IPs are blocked" folklore says would fail):

| Request from the datacenter IP | Result |
|---|---|
| `POST /codex/responses` | `401` JSON ✅ passed |
| `POST /codex/models`, `POST /wham/usage` | `405` (wrong method, but past the edge) ✅ |
| **Same IP, same connection, `+ CF-Worker`** | **`403` HTML** ❌ |

One header flips it on identical infrastructure. So an ordinary cloud VM reaches chatgpt.com fine; only Cloudflare Workers are singled out.

**Fixing it therefore requires egress that is not a Cloudflare Worker**, and any ordinary VM or container will do — Cloud Run, a GCP `e2-micro`, AWS Lambda, or Deno Deploy — forwarding to chatgpt.com while the Worker keeps auth, routing and the account pool. Because the relay terminates the connection and re-originates the request, the CF-* headers stop there. **Not** Vercel Edge: it runs on Cloudflare's own network and reproduces the identical block. (Fly.io was listed here in an earlier revision; its permanent free tier ended in 2024, so it is no longer a free option.)

That egress now exists: the relay was **approved 2026-08-03** and is the one sanctioned exception to the Cloudflare-only rule. Design, wire contract (header allowlist, `x-relay-upstream` / `x-relay-fault` markers, tri-state guard), Cloud Run IAM auth, corrected cost model, and deploy steps live in [codex-relay.md](./codex-relay.md); the constraints originally listed here (streaming byte pipe, per-request credentials with nothing at rest, cost/latency, second egress identity) are folded into that design.

When re-testing the wall itself, send **one** minimal request (tiny prompt, `max_tokens` ≤ 32). Never sweep sizes or loop. A fake token against the upstream is free and reproduces the wall exactly — prefer that over a real request.

## Grok

- Upstream OAuth: SuperGrok. Multi-account pool (product requirement).
- **Surfaces:** both `/openai/v1` and `/anthropic` with model `grok/<upstream_id>` — **different upstream wire formats**:
  - **`/openai/v1`:** near-passthrough Chat Completions to `https://api.x.ai/v1/chat/completions`. Client identity: `User-Agent: grok-shell/<ver> (linux; x86_64)`, `x-grok-client-identifier: grok-shell` (from `xai-org/grok-build`). Always sets `include_reasoning: true`. Plaintext `reasoning_content` is often stripped on Cloudflare egress (see [api.md](./api.md)).
  - **`/anthropic`:** Messages ↔ xAI **Responses** at `https://cli-chat-proxy.grok.com/v1/responses` (OAuth CLI chat-proxy). Client identity aligned with the working CLI OAuth surface: `User-Agent: xai-grok-workspace/<ver>`, `x-grok-client-version: <ver>`, `X-XAI-Token-Auth: xai-grok-cli`. Sends `include: ["reasoning.encrypted_content"]` when thinking is not disabled; maps `encrypted_content` ↔ Anthropic `thinking.signature` (stream `signature_delta`). `cache_control` stripped; sticky ids never invented.
- **Thinking / effort on `/anthropic`:** `thinking.type=disabled` turns off encrypted-reasoning include and suppresses thinking blocks; adaptive/enabled + `output_config.effort` / `reasoning_effort` map to Responses `reasoning.effort` with the `xhigh` ceiling clamp. `budget_tokens` is not mapped. Details in [api.md](./api.md).
- **Reasoning replay cache (Anthropic → grok only).** After a **completed** turn that produced validated `encrypted_content`, this proxy stores a minimal replay item in KV (`CACHE`, TTL 1h) keyed by SHA-256(`api_key_id` + upstream model + session id). Session id is client `x-grok-conv-id`, else `x-grok-session-id`. Match uses a SHA-256 of the trailing assistant plaintext (plaintext is not stored). On the next turn of the same session+model, if the client omitted/stripped `thinking.signature` but the assistant-text hash still matches, the proxy injects `{type:"reasoning", encrypted_content}` into Responses `input`. Completed turns with thinking disabled or no replayable ciphertext **clear** the prior entry. Writes/deletes are scheduled with `waitUntil` so they survive Response close. **KV is eventually consistent** — under load a put may not be visible on the immediate next turn; clients that echo `thinking.signature` do not depend on the cache. No session header ⇒ cache is a no-op. Keys are never shared across API keys, users, or models. On upstream opaque decode failure (compaction blob / encrypted_content), the cache entry is deleted before the strip-and-retry recovery described in [api.md](./api.md).
- **The tool-call loop guard (see [api.md](./api.md)) stays on** for `/anthropic` → grok (conversion path, not Claude-native passthrough).
- **Sampling:** `temperature` forwarded when the client sends it, else pinned to `1`. `top_p` only when sent.
- `reasoning_effort` / Responses `reasoning.effort` with ceiling clamp to `xhigh` — see [api.md](./api.md).
- Sticky header `x-grok-conv-id` forwarded when the client supplies it on **either** surface (also `x-grok-session-id`, `x-grok-turn-idx`); `x-grok-req-id` generated per Chat Completions request. **Never synthesize a conv id** — see the measurement below.
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
- **Catalog (`catalog/models.ts`):** after the builtin loop, one section per custom provider is appended. `models_mode=manual` returns the stored list directly (never fetches). `models_mode=auto` tries the adapter's `listModels()` with an acquired key, using the **same 1h KV cache** as built-ins (cache key scoped `user+slug`, reusing the identical cache helpers); on failure — or when there is no usable key to query — it falls back to the stored manual list when non-empty, else an empty list. **Never fabricates a catalog.** Ids are always `slug/<upstream_id>`.
- **Admin REST:** `/api/custom-providers` — see [auth.md](./auth.md) for the route table and [admin-ui.md](./admin-ui.md) for the UI.

## Failover order

Within one user’s provider pool only. Never cross users. Applies identically to custom providers (pooled under their slug).

## Catalog

- **Claude Code / Grok:** live upstream model lists when an account is bound.
- **Codex:** empty list (no upstream or third-party list API); see Codex section.
- **Custom providers:** manual list, or live + 1h cache with manual/empty fallback — see the Custom endpoints section above.
- Catalog KV cache TTL is **1h** (`?refresh=true` bypasses): unlike admin-UI usage, `GET /openai/v1/models` / `GET /anthropic/v1/models` are called by API clients with no frontend cache, so the KV layer stays — the long TTL keeps its write volume negligible.
- `GET /openai/v1/models` and `GET /anthropic/v1/models` share the same catalog; ids are always `provider/upstream`.
- Only providers the user can use are queried. Unknown model strings may still be attempted if that provider is bound.
