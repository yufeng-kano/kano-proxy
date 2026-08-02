import { Hono } from "hono"
import { cors } from "hono/cors"
import type { HonoEnv } from "./auth/session"
import { loadSessionUser } from "./auth/session"
import { anthropicRoutes } from "./routes/anthropic"
import { authRoutes } from "./routes/auth"
import { customProviderRoutes } from "./routes/custom_providers"
import { keysRoutes } from "./routes/keys"
import { modelsRoutes } from "./routes/models"
import { openaiRoutes } from "./routes/openai"
import { providerRoutes } from "./routes/providers"

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
app.use("/health", cors())

app.use("*", async (c, next) => {
  c.set("user", null)
  c.set("apiKeyUserId", null)
  c.set("apiKeyId", null)
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  if (loaded) c.set("user", loaded.user)
  await next()
})

app.get("/health", (c) => c.json({ ok: true, service: "kano-proxy" }))

app.route("/api/auth", authRoutes)
app.route("/api/keys", keysRoutes)
app.route("/api/models", modelsRoutes)
app.route("/api/providers", providerRoutes)
app.route("/api/custom-providers", customProviderRoutes)

app.route("/openai/v1", openaiRoutes)
app.route("/anthropic", anthropicRoutes)

app.notFound((c) => c.json({ error: "not found" }, 404))

export default app
