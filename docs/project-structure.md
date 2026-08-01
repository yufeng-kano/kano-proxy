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
      wrangler.toml
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
  package.json           # workspace root
```

## Boundaries

- `routes/*` — HTTP only, thin.
- `providers/*` — upstream transport + usage + OAuth specifics.
- `proxy/*` — format conversion and streaming.
- `pool/*` — provider-agnostic selection/bench.
- Vue: thin `App.vue`; logic in composables/services.
