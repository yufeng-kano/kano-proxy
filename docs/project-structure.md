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
        auth/            # session, google, api keys
        proxy/           # stream helpers, openai↔provider
        providers/       # claude-code, codex, grok
        pool/            # acquire, bench, promote
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
- `providers/*` — upstream transport + usage + OAuth specifics.
- `proxy/*` — format conversion and streaming.
- `pool/*` — provider-agnostic selection/bench.
- Vue: thin `App.vue`; logic in composables/services.
