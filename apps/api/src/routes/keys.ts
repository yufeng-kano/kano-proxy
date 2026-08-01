import { Hono } from "hono"
import type { Context } from "hono"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import { createKey, deleteKey, listKeys } from "../db/keys"
import type { UserRow } from "../db/users"

export const keysRoutes = new Hono<HonoEnv>()

async function requireUser(c: Context<HonoEnv>): Promise<UserRow | null> {
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  if (!loaded) return null
  return loaded.user
}

keysRoutes.get("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const keys = await listKeys(c.env.DB, user.id)
  return c.json({
    keys: keys.map((k) => ({
      id: k.id,
      name: k.name,
      key_prefix: k.key_prefix,
      created_at: k.created_at,
      last_used_at: k.last_used_at,
    })),
  })
})

keysRoutes.post("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const body = (await c.req.json().catch(() => ({}))) as { name?: string }
  const name = body.name?.trim() || "default"
  const { row, plaintext } = await createKey(c.env.DB, user.id, name)
  return c.json({
    id: row.id,
    name: row.name,
    key_prefix: row.key_prefix,
    created_at: row.created_at,
    key: plaintext, // once
  })
})

keysRoutes.delete("/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const ok = await deleteKey(c.env.DB, user.id, c.req.param("id"))
  if (!ok) return c.json({ error: "not found" }, 404)
  return c.json({ ok: true })
})
