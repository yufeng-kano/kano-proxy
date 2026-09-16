# Project structure

```text
kano-proxy/
  LICENSE                # MIT
  apps/
    api/                 # the backend: its own Rust workspace, nothing above it
      Cargo.toml         # workspace over crates/*
      Cargo.lock
      crates/
        kano-proxy-core/ # the library every edition composes; its Cargo.toml version is the release version
          migrations/    # 0001_core_baseline.sql onward, tracked in core_migrations
          src/
            lib.rs       # what an edition imports
            app.rs       # build_router(AppState, Extensions)
            extensions.rs # request policy, extra routes, pool extension — passed, never registered
            config.rs    # environment -> CoreConfig
            db/          # pool, migration runner, one module per table
            crypto/      # credential encryption, key hashing, session signing
            auth/        # sessions, Google OIDC, API keys, provider OAuth
            routes/      # every HTTP surface: the LLM bases, the admin API, /agent/v1
            proxy/       # protocol conversion, SSE handling, dispatch and the candidate walk
            providers/   # claude-code, codex, grok, antigravity, and the two BYO adapters
            routing/     # candidates -> facts -> strategy -> feedback: who to try, in what order
            pool/        # bench, promote, credential persistence, the pool-extension contract
            tunnel/      # the in-process registry and WebSocket half of the agent tunnel
            catalog/     # the model list for one user's bound providers
            pricing/     # per-request cost estimation
            logging/     # request log rows and token usage capture
            maintenance/ # the daily retention sweep
            changelog/   # release notes cache and version comparison
        kano-proxy-api/  # the standalone binary
    cli/                 # the kano-proxy CLI — its own crate and release pipeline
    web/                 # Vue + Vite, built to static files
      public/            # robots.txt, _headers (noindex except /docs/* and /login)
      src/
        main.ts          # standalone web assembly
        bootstrap.ts     # createWebApp: app, router and extension composition
        core.ts          # supported web exports for composing editions
        extensions.ts    # route/navigation contracts and injection
        router/, pages/, components/, composables/, i18n/, services/, types/
      package.json, pnpm-lock.yaml
    docs/                # the public documentation site — VitePress (docs/docs-site.md)
      .vitepress/        # site.ts (shared config), config.ts (standalone), theme/
      pages/             # English pages and their Traditional Chinese twins under zh-TW/
      package.json, pnpm-lock.yaml
  docs/                  # this documentation set
  scripts/               # release helpers
  .github/workflows/     # ci (push/PR), cli-release (cli-v* Release)
  .rule                  # canonical repository guardrails
  AGENTS.md, CLAUDE.md   # symlinks to .rule
  .cursor/rules/kano-proxy.mdc # symlink to ../../.rule
  .local.example/        # committed templates for private operator data
  .local/                # gitignored: real DNS, host, deploy notes (not open-source)
```

Every app owns its own manifest, lockfile and build, and nothing language-specific sits at the repository root. A Rust library lives under its workspace's `crates/`; a deployable binary is an app.

## Boundaries

- `routes/*` — HTTP only, thin.
- `providers/*` — upstream transport + usage + OAuth specifics. The two custom-endpoint adapters are built fresh per request from a `custom_providers` row — never added to the static builtin registry.
- `proxy/*` — format conversion and streaming. `dispatch_walk` is the **only** candidate loop: it plans, acquires, fetches (first-byte timeout, one 529 retry), asks `routing/feedback` whether to fail over, cancels superseded upstream bodies, and returns a `WalkOutcome` (`no_account` / `unavailable` / `exhausted` / `fetch_error` / `cancelled` / `response`) — it never decides who to try and never touches the client response. `dispatch` holds the two transports that turn an outcome into a client response: eager (walk runs inside the already-committed SSE stream, failures become terminal frames, one log row on close) and non-stream (walk runs first, HTTP status mirrors the outcome, `Retry-After` recomputed on exhaustion). Everything protocol-specific — SSE error/stall frames, JSON error envelopes, error-type extraction, usage sniffer/parser — lives behind the `Wire` interface (`wire_openai`, `wire_anthropic`); transports never branch on protocol. Audio transcriptions and the Anthropic→OpenAI conversion path are separate modules that reuse the walk, not copies of it.
- `routing/*` — the single owner of account/target selection (docs/providers.md § Routing module): expand → flat candidate list, usability facts, ordering strategy, outcome penalties. Used identically by a group alias and a direct `provider/model` call.
- `pool/*` — bench and credential persistence; provider-agnostic. `extension.rs` holds the **optional** `PoolExtension` contract an edition may pass to `build_router` ([cloud-edition.md](./cloud-edition.md) § Pool extension) — types plus the settle helper, no behavior; the core imports it only as a parameter threaded from the request context, never as a module-level registry.
- Vue: thin `App.vue`; logic in composables/services.
- `apps/docs` — content only. No calls to `/api/*`, no session awareness, no shared code with `apps/web` beyond being copied into its `dist/`. The one piece of script is the origin fill ([docs-site.md](./docs-site.md)).

## Edition composition

The dependency direction is private edition → public core. This repository must not import cloud code or enforce the hosted Free/Paddle policy in its standalone entry. Keep provider/protocol fixes here; billing and cloud-only pages belong in the private repository. Rules for the two repositories are maintained independently; each repository's instruction links point to its own `.rule`.

The `kano-proxy-core` crate exports `build_router`, `AppState`, `CoreConfig` and the extension traits (`RequestPolicy`, `PoolExtension`, and the row and candidate types their signatures name). `kano-proxy-api` is the standalone binary. `build_router` accepts instance-local routes, an authenticated API-key request policy and an optional pool extension, all passed at composition time and carried per request, so two routers built from the same source never see each other's. The policy wraps all API-key routes, including group mounts; editions decide which operations to meter.

`apps/web/src/core.ts` exports `createWebApp`, `createAppRouter`, the authenticated API client, and extension types. Routes and sidebar items are passed when constructing the app. Extension route titles use `meta.title`; core routes retain catalog-backed `meta.titleKey`. The web source currently requires its documented `@` alias to point to the core web `src` directory in the composing Vite/TypeScript config.

The web composition entry also exports `useAuth`, `PageHeader`, and `AppButton` so edition pages can reuse the session state and standard page chrome without importing internal files.

Extension navigation icons are Vue components supplied by the composing edition. New edition navigation does not require adding private feature names to the public icon registry.

`WebExtensions.accountMenu` places edition destinations in the shell's account menu instead of the primary sidebar nav, using the same `{ name, to, label, icon }` shape as navigation items. The core ships none; the menu renders the signed-in address and sign-out on its own.
