import { Hono } from "hono"
import { cors } from "hono/cors"
import type { HonoEnv } from "./auth/session"
import { loadSessionUser } from "./auth/session"
import { anthropicRoutes } from "./routes/anthropic"
import { authRoutes } from "./routes/auth"
import { keysRoutes } from "./routes/keys"
import { modelsRoutes } from "./routes/models"
import { openaiRoutes } from "./routes/openai"
import { providerRoutes } from "./routes/providers"

const app = new Hono<HonoEnv>()

app.use(
  "*",
  cors({
    origin: (origin) => origin || "*",
    credentials: true,
  }),
)

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

app.route("/openai/v1", openaiRoutes)
app.route("/anthropic", anthropicRoutes)

app.notFound((c) => c.json({ error: "not found" }, 404))

export default app
