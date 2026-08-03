# Testing

## Cost safety (hard rule)

Real upstream traffic is real money. **Never** debug, reproduce, bisect, or benchmark by sending real requests through the deployed proxy or directly to paid upstreams — verification is unit tests with stubbed `fetch`. A live smoke test requires the operator's explicit per-instance approval and must be minimal (tiny prompt, `max_tokens` ≤ 32, no long-context/`[1M]` variants, never in a loop or size sweep). For production failures, use free evidence first: `request_logs` in D1 (`wrangler d1 execute --remote`), `wrangler tail`, Cloudflare dashboards.

## Layers

1. **Unit** — pure adapters (OpenAI↔Anthropic mapping, reasoning_effort map, model parse, key hash).
2. **Pool** — acquire/bench/promote with mocked KV/D1.
3. **Route** — Workers + `cloudflare:test` or miniflare-style where available; mock upstream fetch.
4. **Relay** — `apps/relay` Deno tests: header allowlist (CF-* never forwarded), streaming no-buffer proof, path/method fault contract. Stubbed fetch only.
5. **Manual / local wrangler** — `wrangler dev` + curl OpenAI/Anthropic smoke.
6. **No real secrets in CI** — fixtures only.

## Commands

```bash
pnpm test
pnpm --filter api test
pnpm --filter api test:watch
cd apps/relay && deno task test   # egress relay (Deno — not part of pnpm test)
```

## Coding-agent smoke (manual)

```bash
export OPENAI_BASE_URL=http://127.0.0.1:8787/openai/v1
export OPENAI_API_KEY=sk-kano-proxy-...
# chat completions with tools + stream
```

Anthropic:

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8787/anthropic
export ANTHROPIC_API_KEY=sk-kano-proxy-...
```

## Dedicated test owner

Implementation may be split across agents; **one agent owns**:

- Keeping tests green
- Adding regression tests for adapter/cache/auth bugs
- Running `pnpm test` and wrangler local checks after merges
