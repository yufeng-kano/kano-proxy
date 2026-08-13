import { Hono } from "hono"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import { getAccount } from "../db/accounts"
import { listCustomProviders } from "../db/custom_providers"
import {
  countModelGroups,
  deleteModelGroup,
  findAliasConflicts,
  getModelGroupById,
  insertModelGroup,
  listAliasesForGroup,
  listModelGroups,
  parseGroupTargets,
  replaceAliases,
  updateModelGroupFields,
  type GroupTarget,
  type ModelGroupRow,
} from "../db/model_groups"
import { isProviderId } from "../env"
import {
  MAX_MODEL_GROUPS_PER_USER,
  validateAliases,
  validateDisplayName,
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

/**
 * A pinned `account_id` must be an `upstream_accounts` row owned by the
 * caller whose `provider` matches the target's prefix (docs/auth.md §
 * Model groups) — never another user's row, and never a row that quietly
 * belongs to a different provider than the target claims.
 */
function accountResolver(db: D1Database, userId: string): (accountId: string, provider: string) => Promise<boolean> {
  return async (accountId, provider) => {
    const row = await getAccount(db, userId, accountId)
    return !!row && row.provider === provider
  }
}

/**
 * Read-time display label for a pinned target — `custom_label` wins over
 * upstream `label` (same convention as the accounts list resolver), `null`
 * when unpinned or the account no longer exists. Never stored.
 */
async function accountLabel(db: D1Database, userId: string, accountId: string | null): Promise<string | null> {
  if (!accountId) return null
  const row = await getAccount(db, userId, accountId)
  if (!row) return null
  return row.custom_label || row.label || null
}

async function toListItem(
  db: D1Database,
  userId: string,
  row: ModelGroupRow,
): Promise<Record<string, unknown>> {
  const aliasRows = await listAliasesForGroup(db, row.id)
  const targets = parseGroupTargets(row.targets_json)
  const enriched = await Promise.all(
    targets.map(async (t) => ({
      model: t.model,
      account_id: t.account_id,
      account_label: await accountLabel(db, userId, t.account_id),
    })),
  )
  return {
    id: row.id,
    name: row.name,
    aliases: aliasRows.map((a) => a.alias),
    targets: enriched,
    created_at: row.created_at,
    updated_at: row.updated_at,
  }
}

modelGroupRoutes.get("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const rows = await listModelGroups(c.env.DB, user.id)
  const groups = await Promise.all(rows.map((r) => toListItem(c.env.DB, user.id, r)))
  return c.json({ groups })
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
  const nameErr = validateDisplayName(name)
  if (nameErr) return c.json({ error: nameErr }, 400)

  const aliasesRes = validateAliases(body.aliases)
  if (!aliasesRes.ok) return c.json({ error: aliasesRes.error }, 400)
  // Cross-group uniqueness: no group of this user's yet owns any of these
  // aliases (a brand-new group has nothing of its own to exclude).
  const conflicts = await findAliasConflicts(c.env.DB, user.id, aliasesRes.aliases)
  if (conflicts.length > 0) {
    return c.json({ error: `alias "${conflicts[0]}" is already used by another of your groups` }, 400)
  }

  const resolvePrefix = await prefixResolver(c.env.DB, user.id)
  const resolveAccount = accountResolver(c.env.DB, user.id)
  const targetsRes = await validateGroupTargets(body.targets, resolvePrefix, resolveAccount)
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
  await replaceAliases(c.env.DB, { userId: user.id, groupId: row.id, aliases: aliasesRes.aliases })
  return c.json(await toListItem(c.env.DB, user.id, row), 201)
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
    const err = validateDisplayName(name)
    if (err) return c.json({ error: err }, 400)
    const rows = await listModelGroups(c.env.DB, user.id)
    if (rows.some((g) => g.id !== id && g.name === name)) {
      return c.json({ error: `a model group named "${name}" already exists` }, 400)
    }
  }

  let aliases: string[] | undefined
  if (body.aliases !== undefined) {
    const res = validateAliases(body.aliases)
    if (!res.ok) return c.json({ error: res.error }, 400)
    // Exclude this group's own id — replacing a group's aliases with (a
    // superset of) what it already had must never false-positive.
    const conflicts = await findAliasConflicts(c.env.DB, user.id, res.aliases, id)
    if (conflicts.length > 0) {
      return c.json({ error: `alias "${conflicts[0]}" is already used by another of your groups` }, 400)
    }
    aliases = res.aliases
  }

  let targets: GroupTarget[] | undefined
  if (body.targets !== undefined) {
    const resolvePrefix = await prefixResolver(c.env.DB, user.id)
    const resolveAccount = accountResolver(c.env.DB, user.id)
    const res = await validateGroupTargets(body.targets, resolvePrefix, resolveAccount)
    if (!res.ok) return c.json({ error: res.error }, 400)
    targets = res.targets
  }

  await updateModelGroupFields(c.env.DB, id, { name, targets })
  if (aliases) {
    await replaceAliases(c.env.DB, { userId: user.id, groupId: id, aliases })
  }
  const updated = await getModelGroupById(c.env.DB, user.id, id)
  return c.json(await toListItem(c.env.DB, user.id, updated ?? existing))
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
