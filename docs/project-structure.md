# Project structure

```text
kano-proxy/
  LICENSE                # MIT
  README.md, README.zh-TW.md
  docker-compose.yaml    # the four-container self-hosted stack (docs/deployment.md)
  .env.example           # the variables a compose deployment sets, with placeholders (docs/deployment.md)
  apps/
    api/                 # the backend: its own Rust workspace, nothing above it
      Cargo.toml         # workspace over crates/*
      Cargo.lock
      crates/
        kano-proxy-core/ # the library every edition composes; its Cargo.toml version is the release version
          migrations/    # 0001_core_baseline.sql onward, tracked in core_migrations
          src/
            lib.rs       # what an edition imports
            app.rs       # AppState and its builder, build_router(AppState, Extensions), serve
            extensions.rs # the request-policy trait, the API-key identity, extra and shadowed routes — passed, never registered
            config.rs    # environment -> CoreConfig
            cache.rs     # the in-process TTL cache
            ids.rs       # id and timestamp helpers
            http/        # API error envelopes, CORS
            upstream/    # outbound HTTP transport (stubbed in tests)
            db/          # pool, migration runner, one query module per table or table family
            crypto/      # credential encryption, key hashing, session signing
            auth/        # sessions, Google OIDC, API keys, provider OAuth
            routes/      # every HTTP surface: the LLM bases, the admin API, /cli/v1
            proxy/       # protocol conversion, SSE handling, dispatch and the candidate walk
            providers/   # claude-code, codex, grok, antigravity, the two BYO adapters and the CLI tunnel adapter
            routing/     # candidates -> facts -> strategy -> feedback: who to try, in what order
            pool/        # acquire, bench, sign-in state, credential persistence, the pool-extension contract
            tunnel/      # the in-process registry and WebSocket half of the CLI tunnel
            catalog/     # the model list for one user's bound providers
            pricing/     # per-request cost estimation
            logging/     # request log rows and token usage capture
            maintenance/ # the daily sweep: retention, then the price table refresh
            changelog/   # release notes cache and version comparison
            utils/       # model-id, group, custom-endpoint, URL, reasoning, audio and loop-guard helpers
        kano-proxy-api/  # the standalone binary
    cli/                 # the kano-proxy CLI — its own crate and release pipeline
    web/                 # Vue + Vite, built to static files
      public/            # robots.txt (the noindex header is nginx's, deploy/web/nginx.conf)
      src/
        main.ts          # standalone web assembly
        bootstrap.ts     # createWebApp: app, router and extension composition
        core.ts          # supported web exports for composing editions
        extensions.ts    # route/navigation contracts and injection
        router/, pages/, components/, composables/, config/, i18n/, services/, types/
      tests/             # vitest, node environment (`pnpm test`)
      package.json, pnpm-lock.yaml
    docs/                # the public documentation site — VitePress (docs/docs-site.md)
      .vitepress/        # site.ts (shared config), config.ts (standalone), llms.ts (llms.txt), theme/
      pages/             # English pages and their Traditional Chinese twins under zh-TW/
      package.json, pnpm-lock.yaml
  deploy/                # one Dockerfile per service: api/, web/ (nginx.conf, public-origin.sh), caddy/ (Caddyfile)
  docs/                  # this documentation set
  scripts/               # install-cli.sh (the published CLI installer), ci/ (the CLI release's tap and bucket bump)
  .github/workflows/     # ci (PR), cli-release (cli-v* Release)
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
- `proxy/*` — format conversion and streaming. `dispatch_walk` is the **only** candidate loop: it plans, acquires, fetches (first-byte timeout, one 529 retry), asks `routing/feedback` whether to fail over, cancels superseded upstream bodies, and returns a `WalkOutcome` (`NoAccount` / `Unavailable` / `Exhausted` / `FetchError` / `Cancelled` / `Response`) — it never decides who to try and never touches the client response. `dispatch` holds the two transports that turn an outcome into a client response: eager (walk runs inside the already-committed SSE stream, failures become terminal frames, one log row on close) and non-stream (walk runs first, HTTP status mirrors the outcome, `Retry-After` recomputed on exhaustion). Everything protocol-specific — SSE error/stall frames, JSON error envelopes, error-type extraction, usage sniffer/parser — lives behind the `Wire` interface (`wire_openai`, `wire_anthropic`); transports never branch on protocol. Audio transcriptions and the Anthropic→OpenAI conversion path are separate modules that reuse the walk, not copies of it.
- `routing/*` — the single owner of account/target selection (docs/providers.md § Routing module): expand → flat candidate list, usability facts, ordering strategy, outcome penalties. Used identically by a group alias and a direct `provider/model` call.
- `pool/*` — bench, sign-in state (`sign_in.rs`) and credential persistence; provider-agnostic. `extension.rs` holds the **optional** `PoolExtension` contract an edition may pass to `AppStateBuilder::pool_extension` ([cloud-edition.md](./cloud-edition.md) § Pool extension) — the types plus the helpers every core reader shares (lease settlement, the shared custom-endpoint lookup, the borrower header filter); the core reaches the extension only through `AppState`, never as a module-level registry.
- Vue: thin `App.vue`; logic in composables/services.
- `apps/docs` — content only. No calls to `/api/*`, no session awareness, no shared code with `apps/web` beyond being copied into its `dist/`. The one piece of script is the origin fill ([docs-site.md](./docs-site.md)).

## Edition composition

The dependency direction is private edition → public core. This repository must not import cloud code or enforce the hosted Free/Paddle policy in its standalone entry. Keep provider/protocol fixes here; billing and cloud-only pages belong in the private repository. Rules for the two repositories are maintained independently; each repository's instruction links point to its own `.rule`.

The `kano-proxy-core` crate exports `build_router`, `serve`, `AppState`, `CoreConfig`, `Extensions` and the extension traits (`RequestPolicy`, `PoolExtension`, and the row and candidate types their signatures name). `kano-proxy-api` is the standalone binary. `AppState::builder` takes the authenticated API-key request policy, an optional pool extension, the service name and the terms date; `build_router` takes that state plus instance-local routes and shadowed core paths. All are passed at composition time and carried per instance, so two routers built from the same source never see each other's. The policy wraps all API-key routes, including group mounts; editions decide which operations to meter.

`apps/web/src/core.ts` exports `createWebApp`, the authenticated API client (`apiRequest`), the extension types, and what edition pages reuse instead of importing internal files: `useAuth`, `useConfirm`, `PageHeader`, `AppButton`, `SettingsGroup` and `SettingsRow`. Routes and sidebar items are passed when constructing the app. Extension route titles use `meta.title`; core routes retain catalog-backed `meta.titleKey`. The web source requires its documented `@` alias to point to the core web `src` directory in the composing Vite/TypeScript config.

Extension navigation icons are Vue components supplied by the composing edition. New edition navigation does not require adding private feature names to the public icon registry.

`WebExtensions.accountMenu` places edition destinations in the shell's account menu instead of the primary sidebar nav, using the same `{ name, to, label, icon }` shape as navigation items. The core ships none; the menu renders the signed-in address and sign-out on its own.
