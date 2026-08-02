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
        auth/            # session, google, api keys
        proxy/           # stream helpers, openai↔provider
        providers/       # claude-code, codex, grok (builtin registry) +
                          #   custom_openai.ts / custom_anthropic.ts (per-request
                          #   adapters for user-defined endpoints, not in the
                          #   registry) + resolve.ts (model id → adapter)
        pool/            # acquire, bench, promote (builtin id or custom slug)
        db/              # D1 access
        crypto/          # token encryption, key hash
        logging/
      migrations/
      wrangler.toml                 # public placeholders (local dev)
      wrangler.production.example.toml
      wrangler.production.toml      # gitignored: real D1/KV + prod host
      package.json
    web/                 # Vue + Vite → Pages
      src/
        pages/
        components/
        composables/
        services/
        types/
      package.json
  packages/
    shared/              # shared types (optional)
  docs/
  scripts/
    ci/                  # CI helpers (e.g. write production wrangler from env)
  .github/workflows/     # release-deploy on GitHub Release publish
  .local.example/        # committed templates for private operator data
  .local/                # gitignored: real DNS, host, deploy notes (not open-source)
  package.json           # workspace root
```

## Boundaries

- `routes/*` — HTTP only, thin.
- `providers/*` — upstream transport + usage + OAuth specifics. The two custom-endpoint adapters are factories (`createCustomOpenAIAdapter(row)` / `createCustomAnthropicAdapter(row)`), built fresh per request from a `custom_providers` D1 row — never added to the static builtin registry in `providers/index.ts`.
- `proxy/*` — format conversion and streaming.
- `pool/*` — provider-agnostic selection/bench.
- Vue: thin `App.vue`; logic in composables/services.
