import { Hono } from "hono"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import { listCustomProviders } from "../db/custom_providers"
import {
  countModelGroups,
  deleteModelGroup,
  getModelGroupById,
  insertModelGroup,
  listModelGroups,
  parseGroupTargets,
  updateModelGroupFields,
  type ModelGroupRow,
} from "../db/model_groups"
import { isProviderId } from "../env"
import {
  MAX_MODEL_GROUPS_PER_USER,
  validateGroupName,
  validateGroupTargets,
} from "../utils/model_group"

export const modelGroupRoutes = new Hono<HonoEnv>()

async function requireUser(c: {
  env: HonoEnv["Bindings"]
  req: { header: (n: string) => string | undefined }
}) {
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  return loaded?.user ?? null
}

/** Builtin `ProviderId`, or one of the caller's own custom provider slugs — never another user's. */
async function prefixResolver(db: D1Database, userId: string): Promise<(prefix: string) => boolean> {
  const rows = await listCustomProviders(db, userId)
  const slugs = new Set(rows.map((r) => r.slug))
  return (prefix: string) => isProviderId(prefix) || slugs.has(prefix)
}

function toListItem(row: ModelGroupRow): Record<string, unknown> {
  return {
    id: row.id,
    name: row.name,
    targets: parseGroupTargets(row.targets_json),
    created_at: row.created_at,
    updated_at: row.updated_at,
  }
}

modelGroupRoutes.get("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const rows = await listModelGroups(c.env.DB, user.id)
  return c.json({ groups: rows.map(toListItem) })
})

modelGroupRoutes.post("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)

  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid JSON" }, 400)
  }

  const name = typeof body.name === "string" ? body.name.trim() : ""
  const nameErr = validateGroupName(name)
  if (nameErr) return c.json({ error: nameErr }, 400)

  const resolvePrefix = await prefixResolver(c.env.DB, user.id)
  const targetsRes = validateGroupTargets(body.targets, resolvePrefix)
  if (!targetsRes.ok) return c.json({ error: targetsRes.error }, 400)

  const count = await countModelGroups(c.env.DB, user.id)
  if (count >= MAX_MODEL_GROUPS_PER_USER) {
    return c.json({ error: `maximum of ${MAX_MODEL_GROUPS_PER_USER} model groups reached` }, 400)
  }

  const existing = await listModelGroups(c.env.DB, user.id)
  if (existing.some((g) => g.name === name)) {
    return c.json({ error: `a model group named "${name}" already exists` }, 400)
  }

  const row = await insertModelGroup(c.env.DB, {
    userId: user.id,
    name,
    targets: targetsRes.targets,
  })
  return c.json(toListItem(row), 201)
})

modelGroupRoutes.put("/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const id = c.req.param("id")
  const existing = await getModelGroupById(c.env.DB, user.id, id)
  if (!existing) return c.json({ error: "not found" }, 404)

  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid JSON" }, 400)
  }

  let name: string | undefined
  if (body.name !== undefined) {
    name = typeof body.name === "string" ? body.name.trim() : ""
    const err = validateGroupName(name)
    if (err) return c.json({ error: err }, 400)
    const rows = await listModelGroups(c.env.DB, user.id)
    if (rows.some((g) => g.id !== id && g.name === name)) {
      return c.json({ error: `a model group named "${name}" already exists` }, 400)
    }
  }

  let targets: string[] | undefined
  if (body.targets !== undefined) {
    const resolvePrefix = await prefixResolver(c.env.DB, user.id)
    const res = validateGroupTargets(body.targets, resolvePrefix)
    if (!res.ok) return c.json({ error: res.error }, 400)
    targets = res.targets
  }

  await updateModelGroupFields(c.env.DB, id, { name, targets })
  const updated = await getModelGroupById(c.env.DB, user.id, id)
  return c.json(toListItem(updated ?? existing))
})

modelGroupRoutes.delete("/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const id = c.req.param("id")
  const existing = await getModelGroupById(c.env.DB, user.id, id)
  if (!existing) return c.json({ error: "not found" }, 404)
  await deleteModelGroup(c.env.DB, user.id, id)
  return c.json({ ok: true })
})
