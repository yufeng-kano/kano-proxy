# Project structure

```text
kano-proxy/
  apps/
    api/                 # Cloudflare Worker
      src/
        index.ts         # route wiring
        env.ts
        routes/
          openai.ts
          anthropic.ts
          auth.ts
          keys.ts
          providers.ts
          custom_providers.ts    # admin REST for user-defined BYO endpoints
          agent.ts               # /agent/v1: CLI device auth + tunnel connect (docs/cli.md)
          cli.ts                 # /api/cli: session-side CLI device/provider management
        auth/            # session, google, api keys
        proxy/           # stream helpers, openai↔provider
        providers/       # claude-code, codex, grok, antigravity (builtin registry) +
                          #   custom_openai.ts / custom_anthropic.ts (per-request
                          #   adapters for user-defined endpoints, not in the
                          #   registry)
        routing/         # candidates.ts (expand → flat candidate list),
                          #   facts.ts (bench + usage-window usability),
                          #   strategy.ts (order candidates), feedback.ts
                          #   (outcome → penalty) — single owner of
                          #   account/target selection (docs/providers.md
                          #   § Routing module)
        pool/            # bench, promote (builtin id or custom slug);
                          #   credential persistence (saveCredential)
        do/              # AgentTunnel Durable Object + wire protocol (docs/cli.md)
        db/              # D1 access
        crypto/          # token encryption, key hash
        logging/
      migrations/
      wrangler.toml                 # public placeholders (local dev)
      wrangler.production.example.toml
      wrangler.production.toml      # gitignored: real D1/KV + prod host
      package.json
    relay/               # Codex egress relay — Deno on Cloud Run (the one approved
                          #   non-Cloudflare piece; see docs/codex-relay.md).
                          #   No package.json on purpose: not a pnpm workspace member.
      main.ts            # Deno.serve entry (PORT, upstream base)
      relay.ts           # handler factory: allowlist, pipe, markers
      relay_test.ts
      deno.json          # tasks: test / check / start
      Dockerfile
    cli/                 # kano-proxy CLI — Rust/Cargo (docs/cli.md; the one
                          #   Rust component; target/ is gitignored)
    web/                 # Vue + Vite → Pages
      public/            # robots.txt, _headers (noindex except /docs/* and /login) — no _redirects
      src/
        pages/
        components/
          ui/            # shared primitives (shell, header, table, modal…)
        composables/
        i18n/            # message catalog + translation runtime
        services/
        types/
      package.json
    docs/                # Public documentation site — VitePress (docs/docs-site.md).
                          #   Built into apps/web/dist/docs/ by root `pnpm build:site`,
                          #   served at /docs/ from the same Pages project.
      .vitepress/
        config.ts        # base /docs/, locales (root en + zh-TW), sidebar, local search
        theme/           # default theme + origin fill (<your-domain> → location.host)
      *.md               # English pages (reference tree)
      zh-TW/             # Traditional Chinese pages, same file names
      package.json
  packages/
    shared/              # shared types (optional)
  docs/
  scripts/
    ci/                  # CI helpers (e.g. write production wrangler from env)
  .github/workflows/     # ci (PRs), release-deploy (v* Release), cli-release (cli-v* Release)
  .local.example/        # committed templates for private operator data
  .local/                # gitignored: real DNS, host, deploy notes (not open-source)
  package.json           # workspace root
```

## Boundaries

- `routes/*` — HTTP only, thin.
- `providers/*` — upstream transport + usage + OAuth specifics. The two custom-endpoint adapters are factories (`createCustomOpenAIAdapter(row)` / `createCustomAnthropicAdapter(row)`), built fresh per request from a `custom_providers` D1 row — never added to the static builtin registry in `providers/index.ts`.
- `proxy/*` — format conversion and streaming; `dispatch.ts` walks the candidate list `routing/*` hands it and reports each attempt's outcome back (bench penalty), but never decides who to try.
- `routing/*` — the single owner of account/target selection (docs/providers.md § Routing module): expand → flat candidate list, usability facts, ordering strategy, outcome penalties. Used identically by a group alias and a direct `provider/model` call.
- `pool/*` — bench (KV) and credential persistence; provider-agnostic.
- `apps/relay` — dumb byte pipe only: no auth logic (Cloud Run IAM fronts it), no state, no format awareness, no credentials at rest. Anything smarter belongs in the Worker.
- Vue: thin `App.vue`; logic in composables/services.
- `apps/docs` — content only. No calls to `/api/*`, no session awareness, no shared code with `apps/web` beyond being copied into its `dist/`. The one piece of script is the origin fill ([docs-site.md](./docs-site.md)).
