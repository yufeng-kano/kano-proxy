# Testing

## Layers

1. **Unit** — pure adapters (OpenAI↔Anthropic mapping, reasoning_effort map, model parse, key hash).
2. **Pool** — acquire/bench/promote with mocked KV/D1.
3. **Route** — Workers + `cloudflare:test` or miniflare-style where available; mock upstream fetch.
4. **Manual / local wrangler** — `wrangler dev` + curl OpenAI/Anthropic smoke.
5. **No real secrets in CI** — fixtures only.

## Commands

```bash
pnpm test
pnpm --filter api test
pnpm --filter api test:watch
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
