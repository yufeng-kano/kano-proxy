import { Hono } from "hono"
import type { Context } from "hono"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import { listModelsForUser } from "../catalog/models"
import type { UserRow } from "../db/users"

export const modelsRoutes = new Hono<HonoEnv>()

async function requireUser(c: Context<HonoEnv>): Promise<UserRow | null> {
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  return loaded?.user ?? null
}

/** Public LLM base host: request origin (custom domain / local Worker). */
function publicApiOrigin(c: Context<HonoEnv>): string {
  try {
    return new URL(c.req.url).origin
  } catch {
    return (c.env.APP_URL || "").replace(/\/$/, "")
  }
}

/**
 * Admin UI: models from live upstream APIs for providers the user has bound.
 * ?refresh=true bypasses 90s KV cache.
 */
modelsRoutes.get("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const force = c.req.query("refresh") === "true"
  const { models, providers } = await listModelsForUser(c.env, user.id, { force })
  const origin = publicApiOrigin(c)
  return c.json({
    object: "list",
    data: models,
    providers: providers.map((p) => ({
      provider: p.provider,
      count: p.models.length,
      error: p.error,
      cached: p.cached,
    })),
    openai_base: `${origin}/openai/v1`,
    anthropic_base: `${origin}/anthropic`,
  })
})
