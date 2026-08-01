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
- OpenAI adapter: Messages ↔ Chat Completions (stream, tools, vision, json_schema).
- Anthropic path: body passthrough + auth inject + beta merge.
- **Caching is explicit, unlike Grok/Codex.** Anthropic only caches at `cache_control` breakpoints (max 4/request; min cacheable prefix 512–4096 tokens by model; 5m default / 1h TTL; cache writes cost 1.25×/2×, reads ~0.1×). There is no conv-id or sticky-routing header — placement is the whole mechanism.
  - `/anthropic` path: clients that set their own breakpoints (Claude Code does) keep them through our passthrough, so caching already works end to end. Do not touch them.
  - `/openai/v1` → claude-code path: we never add `cache_control`, so **these requests never cache**. That is deliberate, not an oversight — a breakpoint on a one-off request costs 1.25× and reads back nothing. Adding one only pays off when the same prefix repeats, which the proxy cannot know from a single stateless request. Revisit only with a measured repeat-rate, and document the placement rule here first.

## Codex

- Upstream: ChatGPT `codex/responses` (reverse-engineered; headers from lincy).
- OpenAI adapter: Chat Completions ↔ Responses SSE.
- **Models:** ChatGPT OAuth has **no** public `/models` endpoint. Platform `GET api.openai.com/v1/models` rejects these tokens (`api.model.read` missing). There is also **no trusted third-party API** that returns per-account Codex OAuth inventory. kano-proxy returns an **empty** list (no hard-coded catalog). Admin UI links official docs: [OpenAI models](https://developers.openai.com/api/docs/models), [ChatGPT / Codex models](https://learn.chatgpt.com/docs/models). Clients may still send a model id if a Codex account is bound; unknown/unsupported ids fail at upstream.
- Usage: dynamic windows (label 5h / Week / Nd) from `/codex/usage` (alias `/wham/usage`).
- Usage fetch: CLI `User-Agent: codex_cli_rs/0.144.3`. chatgpt.com edge **403 bot-challenges by TLS/client fingerprint, not headers** (verified 2026-08-01: same headers/IP → stdlib urllib 401 JSON passes the wall, curl and workerd `fetch` get 403 HTML). lincy passes only because Python urllib's fingerprint is allowed; a Worker cannot change its `fetch` fingerprint, so header tuning cannot fix this. When blocked, account stays **active/standby** (not unusable); UI omits usage bars (chat still works).

## Grok

- Upstream: `api.x.ai` OpenAI-compatible Chat Completions with SuperGrok OAuth.
- Multi-account pool (product requirement; not limited to lincy’s single file).
- `reasoning_effort` passthrough.
- Client identity: present as the official CLI (`User-Agent: grok-shell/<ver> (linux; x86_64)`, `x-grok-client-identifier: grok-shell`), same rationale as codex's `originator: codex_cli_rs` — the subscription OAuth surface is the CLI's, and providers gate on client shape (cf. chatgpt.com bot wall). Shape/version from `xai-org/grok-build` (`xai-grok-sampler/src/client.rs`, `xai-grok-version`). **Not** a billing lever: nothing in that source ties client identity to metering.
- Sticky header `x-grok-conv-id` forwarded when the client supplies it (also `x-grok-session-id`, `x-grok-turn-idx`); `x-grok-req-id` generated per request. **Never synthesize a conv id** — see the measurement below.
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
- `/models` only queries providers the user can use. Unknown model strings may still be attempted if that provider is bound.
