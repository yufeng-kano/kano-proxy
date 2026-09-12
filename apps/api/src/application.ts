import { Hono, type MiddlewareHandler } from "hono"
import { cors } from "hono/cors"
import type { HonoEnv } from "./auth/session"
import { loadSessionUser } from "./auth/session"
import type { Env } from "./env"
import { runRetentionSweep } from "./maintenance/retention"
import type { PoolExtension } from "./pool/extension"
import { ensureFreshPriceTable } from "./pricing/litellm"
import { agentRoutes } from "./routes/agent"
import { anthropicRoutes } from "./routes/anthropic"
import { authRoutes } from "./routes/auth"
import { changelogRoutes } from "./routes/changelog"
import { cliRoutes } from "./routes/cli"
import { customProviderRoutes } from "./routes/custom_providers"
import { groupEndpointRoutes } from "./routes/group_endpoints"
import { keysRoutes } from "./routes/keys"
import { logsRoutes } from "./routes/logs"
import { modelGroupRoutes } from "./routes/model_groups"
import { modelsRoutes } from "./routes/models"
import { openaiRoutes } from "./routes/openai"
import { providerRoutes } from "./routes/providers"
import { usageRoutes } from "./routes/usage"

/** Extensions are local to this app instance; standalone installs need none. */
export interface ApplicationOptions {
  requestPolicy?: MiddlewareHandler<HonoEnv>
  registerRoutes?: (app: Hono<HonoEnv>) => void
  /** Cross-user pool sharing (docs/cloud-edition.md § "Pool extension"). Absent → the core never looks outside the caller's own rows. */
  poolExtension?: PoolExtension
}

export function createApplication(options: ApplicationOptions = {}): Hono<HonoEnv> {
  const app = new Hono<HonoEnv>()

  // /api/*: admin SPA only, cookie-credentialed — origin must match APP_URL.
  app.use(
    "/api/*",
    cors({
      origin: (origin, c) => {
        const appOrigin = (c.env.APP_URL || "").replace(/\/$/, "")
        try {
          return origin && new URL(origin).origin === new URL(appOrigin).origin ? origin : ""
        } catch {
          return ""
        }
      },
      credentials: true,
    }),
  )

  // LLM surfaces + health: any origin, but never credentialed — clients
  // authenticate with a project API key, never the session cookie.
  app.use("/openai/*", cors())
  app.use("/anthropic/*", cors())
  app.use("/g/*", cors())
  app.use("/health", cors())

  app.use("*", async (c, next) => {
    c.set("requestPolicy", options.requestPolicy)
    c.set("poolExtension", options.poolExtension)
    c.set("user", null)
    c.set("apiKeyUserId", null)
    c.set("apiKeyId", null)
    const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
    if (loaded) c.set("user", loaded.user)
    await next()
  })

  options.registerRoutes?.(app)

  app.get("/health", (c) => c.json({ ok: true, service: "kano-proxy" }))

  app.route("/api/auth", authRoutes)
  app.route("/api/keys", keysRoutes)
  app.route("/api/models", modelsRoutes)
  app.route("/api/providers", providerRoutes)
  app.route("/api/custom-providers", customProviderRoutes)
  app.route("/api/cli", cliRoutes)
  app.route("/api/model-groups", modelGroupRoutes)
  app.route("/api/usage", usageRoutes)
  app.route("/api/logs", logsRoutes)
  app.route("/api/changelog", changelogRoutes)

  app.route("/openai/v1", openaiRoutes)
  app.route("/anthropic", anthropicRoutes)
  // CLI device auth + agent tunnel connect (docs/cli.md) — token-authenticated,
  // never session; its own namespace beside /api and the LLM surfaces.
  app.route("/agent/v1", agentRoutes)
  // Model-group virtual endpoints: /g/<slug>/openai/v1/* and
  // /g/<slug>/anthropic/* (docs/api.md § Group endpoints).
  app.route("/g", groupEndpointRoutes)

  app.notFound((c) => c.json({ error: "not found" }, 404))


  return app
}

export function createWorker(app: Hono<HonoEnv>): ExportedHandler<Env> {
  // Cron-triggered retention sweep (see docs/logging.md) — kept out of the
  // request path. A sweep failure is logged and swallowed here so it can never
  // surface as an unhandled rejection in the runtime.
  return {
    fetch: app.fetch,
    scheduled: (event, env, ctx) => {
      ctx.waitUntil(
        runRetentionSweep(env).catch((err) => {
          console.error("[retention] sweep failed:", err)
        }),
      )
      // Daily LiteLLM price-table refresh (docs/pricing.md) — its own failure
      // path (refreshPriceTable never throws), kept apart from the sweep.
      ctx.waitUntil(ensureFreshPriceTable(env))
    },
  }


}
