# Project structure

```text
kano-proxy/
  apps/
    api/                 # Cloudflare Worker
      src/
        index.ts         # standalone Worker assembly
        application.ts   # application/Worker factories, route wiring, edition hooks
        core.ts          # supported API exports for composing editions
        env.ts
        routes/
          openai.ts
          responses.ts           # POST /openai/v1/responses (docs/api.md): native codex passthrough or Responses↔Chat conversion
          anthropic.ts
          auth.ts
          keys.ts
          providers.ts
          custom_providers.ts    # admin REST for user-defined BYO endpoints
          agent.ts               # /agent/v1: CLI device auth + tunnel connect (docs/cli.md)
          cli.ts                 # /api/cli: session-side CLI device/provider management
        auth/            # session, google, api keys
        proxy/           # stream helpers, openai↔provider; responses_openai.ts = Responses API ↔ Chat Completions
                          #   sse_lines.ts = bounded linear line reader; backpressure.ts = demand/cancellation;
                          #   sse_error_rewrite.ts = bounded error detection + byte passthrough; request_json.ts = release raw JSON cache
                          #   dispatch.ts (Chat Completions + Anthropic Messages entry
                          #   points: one eager and one non-stream transport, both
                          #   generic over a `Wire`), dispatch_walk.ts (the single
                          #   candidate walk: plan → acquire → fetch → fail-over →
                          #   `WalkOutcome`), wire.ts + wire_openai.ts / wire_anthropic.ts
                          #   (protocol-specific frames, error bodies, usage parsing),
                          #   dispatch_audio.ts, dispatch_anthropic_via_openai.ts
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
                          #   credential persistence (saveCredential);
                          #   extension.ts (optional cross-user pool contract —
                          #   docs/cloud-edition.md § Pool extension)
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
        main.ts          # standalone web assembly
        bootstrap.ts     # createWebApp: app, router and extension composition
        core.ts          # supported web exports for composing editions
        extensions.ts    # route/navigation contracts and injection
        router/          # createAppRouter; per-app bootstrap state
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
        site.ts          # defineDocsConfig(edition): base /docs/, locales (root en + zh-TW), sidebar, local search
        config.ts        # the standalone site: defineDocsConfig()
        theme/           # default theme + origin fill (<your-domain> → location.host)
      *.md               # English pages (reference tree)
      zh-TW/             # Traditional Chinese pages, same file names
      package.json
  packages/
    shared/              # shared types (optional)
  docs/
  scripts/
    ci/                  # CI helpers (e.g. write production wrangler from env)
  .github/workflows/     # ci (push/PR), retired release-deploy, cli-release (cli-v* Release)
  .rule                 # canonical repository guardrails
  AGENTS.md             # symlink to .rule
  CLAUDE.md             # symlink to .rule
  .cursor/rules/kano-proxy.mdc # symlink to ../../.rule
  .local.example/        # committed templates for private operator data
  .local/                # gitignored: real DNS, host, deploy notes (not open-source)
  package.json           # workspace root
```

## Boundaries

- `routes/*` — HTTP only, thin.
- `providers/*` — upstream transport + usage + OAuth specifics. The two custom-endpoint adapters are factories (`createCustomOpenAIAdapter(row)` / `createCustomAnthropicAdapter(row)`), built fresh per request from a `custom_providers` D1 row — never added to the static builtin registry in `providers/index.ts`.
- `proxy/*` — format conversion and streaming. `dispatch_walk.ts` is the **only** candidate loop: it plans, acquires, fetches (first-byte timeout, one 529 retry), asks `routing/feedback` whether to fail over, cancels superseded upstream bodies, and returns a `WalkOutcome` (`no_account` / `unavailable` / `exhausted` / `fetch_error` / `cancelled` / `response`) — it never decides who to try and never touches the client response. `dispatch.ts` holds the two transports that turn an outcome into a client response: eager (walk runs inside the already-committed SSE stream, failures become terminal frames, one log row on close) and non-stream (walk runs first, HTTP status mirrors the outcome, `Retry-After` recomputed on exhaustion). Everything protocol-specific — SSE error/stall frames, JSON error envelopes, error-type extraction, usage sniffer/parser — lives behind the `Wire` interface (`wire_openai.ts`, `wire_anthropic.ts`); transports never branch on protocol. Audio transcriptions and the Anthropic→OpenAI conversion path are separate files that reuse the walk, not copies of it.
- `routing/*` — the single owner of account/target selection (docs/providers.md § Routing module): expand → flat candidate list, usability facts, ordering strategy, outcome penalties. Used identically by a group alias and a direct `provider/model` call.
- `pool/*` — bench (KV) and credential persistence; provider-agnostic. `extension.ts` holds the **optional** `PoolExtension` contract an edition may pass to `createApplication` ([cloud-edition.md](./cloud-edition.md) § Pool extension) — types plus the settle helper, no behavior; the core imports it only as a parameter threaded from the request context, never as a module-level registry.
- `apps/relay` — dumb byte pipe only: no auth logic (Cloud Run IAM fronts it), no state, no format awareness, no credentials at rest. Anything smarter belongs in the Worker.
- Vue: thin `App.vue`; logic in composables/services.
- `apps/docs` — content only. No calls to `/api/*`, no session awareness, no shared code with `apps/web` beyond being copied into its `dist/`. The one piece of script is the origin fill ([docs-site.md](./docs-site.md)).

## Edition composition

The dependency direction is private edition → public core. This repository must not import cloud code or enforce the hosted Free/Paddle policy in its standalone entry. Keep provider/protocol fixes here; billing and cloud-only pages belong in the private repository. Rules for the two repositories are maintained independently; each repository's instruction links point to its own `.rule`.

`apps/api/src/core.ts` exports `createApplication`, `createWorker`, `AgentTunnel`, public environment types, and the pool-extension contract (`PoolExtension`, `SharedAccount`, `AttemptLease`, plus the `AccountRow` / `RoutingCandidate` / `ProviderId` types its signatures name). `src/index.ts` is the standalone entry. The application factory accepts instance-local route registration, an authenticated API-key request policy, and an optional `poolExtension`. All three are stored per request on the Hono context, so two applications built from the same source never see each other's. The policy wraps all API-key routes, including group mounts; editions decide which operations to meter.

`apps/web/src/core.ts` exports `createWebApp`, `createAppRouter`, the authenticated API client, and extension types. Routes and sidebar items are passed when constructing the app. Extension route titles use `meta.title`; core routes retain catalog-backed `meta.titleKey`. The web source currently requires its documented `@` alias to point to the core web `src` directory in the composing Vite/TypeScript config.

The web composition entry also exports `useAuth`, `PageHeader`, and `AppButton` so edition pages can reuse the session state and standard page chrome without importing internal files.

Extension navigation icons are Vue components supplied by the composing edition. New edition navigation does not require adding private feature names to the public icon registry.

`WebExtensions.accountMenu` places edition destinations in the shell's account menu instead of the primary sidebar nav, using the same `{ name, to, label, icon }` shape as navigation items. The core ships none; the menu renders the signed-in address and sign-out on its own.
