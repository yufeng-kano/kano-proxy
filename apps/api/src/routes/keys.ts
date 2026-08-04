import { Hono } from "hono"
import type { Context } from "hono"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import { isSpendLimitInterval, keyWindowSpend } from "../auth/spend_limit"
import { createKey, deleteKey, listKeys, updateKey, type ApiKeyRow, type SpendLimitFields } from "../db/keys"
import type { UserRow } from "../db/users"

export const keysRoutes = new Hono<HonoEnv>()

async function requireUser(c: Context<HonoEnv>): Promise<UserRow | null> {
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  if (!loaded) return null
  return loaded.user
}

/**
 * Validated limit fields off a request body. `undefined` = the body did not
 * speak to limits at all; an explicit `spend_limit: null` clears the limit.
 * Returns "invalid" (a sentinel, so null stays meaningful) on a bad shape.
 */
function parseLimitFields(body: Record<string, unknown>): SpendLimitFields | undefined | "invalid" {
  const hasAny =
    "spend_limit" in body || "spend_limit_interval" in body || "spend_limit_include_oauth" in body
  if (!hasAny) return undefined

  const rawLimit = body.spend_limit
  let spendLimit: number | null
  if (rawLimit == null) {
    spendLimit = null
  } else if (typeof rawLimit === "number" && Number.isFinite(rawLimit) && rawLimit > 0) {
    spendLimit = rawLimit
  } else {
    return "invalid"
  }

  const rawInterval = body.spend_limit_interval ?? "monthly"
  if (!isSpendLimitInterval(rawInterval)) return "invalid"

  const rawInclude = body.spend_limit_include_oauth ?? true
  if (typeof rawInclude !== "boolean") return "invalid"

  return {
    spendLimit,
    spendLimitInterval: rawInterval,
    spendLimitIncludeOauth: rawInclude,
  }
}

/** Response shape for one key — limit fields always present, spend added by the list route. */
function keyJson(k: ApiKeyRow): Record<string, unknown> {
  return {
    id: k.id,
    name: k.name,
    key_prefix: k.key_prefix,
    created_at: k.created_at,
    last_used_at: k.last_used_at,
    spend_limit: k.spend_limit,
    spend_limit_interval: k.spend_limit_interval,
    spend_limit_include_oauth: k.spend_limit_include_oauth === 1,
  }
}

keysRoutes.get("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const keys = await listKeys(c.env.DB, user.id)
  // Uncached window sums — the admin list is the surface where "how much has
  // this key spent" must be current, and it is a handful of keys at most.
  const withSpend = await Promise.all(
    keys.map(async (k) => ({
      ...keyJson(k),
      window_spend: await keyWindowSpend(c.env, k),
    })),
  )
  return c.json({ keys: withSpend })
})

keysRoutes.post("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const body = (await c.req.json().catch(() => ({}))) as Record<string, unknown>
  const name = (typeof body.name === "string" && body.name.trim()) || "default"
  const limits = parseLimitFields(body)
  if (limits === "invalid") return c.json({ error: "invalid_spend_limit" }, 400)
  const { row, plaintext } = await createKey(c.env.DB, user.id, name, limits)
  return c.json({
    ...keyJson(row),
    key: plaintext, // once
  })
})

keysRoutes.patch("/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const body = (await c.req.json().catch(() => ({}))) as Record<string, unknown>
  const patch: { name?: string; limits?: SpendLimitFields } = {}
  if (body.name !== undefined) {
    if (typeof body.name !== "string" || !body.name.trim()) {
      return c.json({ error: "invalid_name" }, 400)
    }
    patch.name = body.name.trim()
  }
  const limits = parseLimitFields(body)
  if (limits === "invalid") return c.json({ error: "invalid_spend_limit" }, 400)
  if (limits !== undefined) patch.limits = limits
  const ok = await updateKey(c.env.DB, user.id, c.req.param("id"), patch)
  if (!ok) return c.json({ error: "not found" }, 404)
  const keys = await listKeys(c.env.DB, user.id)
  const updated = keys.find((k) => k.id === c.req.param("id"))
  return c.json(updated ? keyJson(updated) : { ok: true })
})

keysRoutes.delete("/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const ok = await deleteKey(c.env.DB, user.id, c.req.param("id"))
  if (!ok) return c.json({ error: "not found" }, 404)
  return c.json({ ok: true })
})
