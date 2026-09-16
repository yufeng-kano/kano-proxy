# Testing

## Cost safety (hard rule)

Real upstream traffic is real money. **Never** debug, reproduce, bisect, or benchmark by sending real requests through the deployed proxy or directly to paid upstreams — verification is unit tests with stubbed `fetch`. A live smoke test requires the operator's explicit per-instance approval and must be minimal (tiny prompt, `max_tokens` ≤ 32, no long-context/`[1M]` variants, never in a loop or size sweep). For production failures, use free evidence first: the `request_logs` table and the server's own logs.

## Layers

1. **Unit** — pure adapters (OpenAI↔Anthropic and Gemini↔OpenAI/Anthropic mapping, reasoning_effort map + custom-openai effort-rejection parsers / `nearestReasoningEffort`, antigravity's 429 classifier, model parse, key hash).
2. **Pool** — acquire/bench/promote against a throwaway database.
3. **Route** — the router built in-process and driven with real requests; the upstream transport is stubbed.
4. **Manual / local** — run the server against a local database and curl the OpenAI/Anthropic surfaces.
5. **No real secrets in CI** — fixtures only.

## Commands

```bash
cd apps/server && cargo test --workspace   # the core crate and the server binary
cd apps/cli    && cargo test               # kano-proxy CLI
cd apps/web    && pnpm install && pnpm test
```

Database-backed tests read `KANO_TEST_DATABASE_URL` (a throwaway PostgreSQL; CI starts one), give each test its own database, and skip themselves when none is reachable.

Every app carries its own manifest and lockfile, so each one installs where it lives; there is no repository-wide `pnpm test`.

The agent tunnel protocol ([cli.md](./cli.md)) is tested at protocol level: the multiplexer is driven with in-memory frames, an end-to-end test drives a real WebSocket client against the registry, and the CLI's protocol and state modules carry their own unit tests. A local LLM is free, but the suite still never assumes one is running.

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
- Running the test suites after merges
