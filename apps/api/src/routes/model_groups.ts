import { Hono } from "hono"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import { getAccount } from "../db/accounts"
import { listCliProviders } from "../db/cli"
import { listCustomProviders } from "../db/custom_providers"
import {
  countModelGroups,
  deleteModelGroup,
  getModelGroupById,
  insertModelGroup,
  listModelsForGroup,
  listModelGroups,
  parseGroupTargets,
  replaceGroupModels,
  updateModelGroupFields,
  type GroupModelInput,
  type GroupTarget,
  type ModelGroupRow,
} from "../db/model_groups"
import { isProviderId, type Env } from "../env"
import { candidatesForTarget, resolveTargetPrefix } from "../routing/candidates"
import { candidateFactsList, earliestUnusableUntil } from "../routing/facts"
import type { CandidateFacts } from "../routing/types"
import { DEFAULT_STRATEGY } from "../routing/strategy"
import {
  MAX_MODEL_GROUPS_PER_USER,
  validateDisplayName,
  validateGroupModels,
  validateGroupSlug,
  validateStrategy,
} from "../utils/model_group"

export const modelGroupRoutes = new Hono<HonoEnv>()

async function requireUser(c: {
  env: HonoEnv["Bindings"]
  req: { header: (n: string) => string | undefined }
}) {
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  return loaded?.user ?? null
}

/** Builtin `ProviderId`, or one of the caller's own custom or CLI provider slugs (docs/cli.md) — never another user's. */
async function prefixResolver(db: D1Database, userId: string): Promise<(prefix: string) => boolean> {
  const rows = await listCustomProviders(db, userId)
  const cliRows = await listCliProviders(db, userId)
  const slugs = new Set([...rows.map((r) => r.slug), ...cliRows.map((r) => r.slug)])
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

type TargetRouting = {
  usable: boolean
  reason: "benched" | "limit" | "unresolved" | "no_account" | null
  unusable_until: string | null
}

function reasonForFacts(facts: CandidateFacts): "benched" | "limit" {
  // A bench that lasts at least as long as the usage window is the effective
  // blocker. This also makes equal expiries deterministic.
  if (facts.benchUntil !== null && (facts.usageWindowUntil === null || facts.benchUntil > facts.usageWindowUntil)) {
    return "benched"
  }
  return "limit"
}

async function routingForTarget(env: Env, userId: string, index: number, target: GroupTarget): Promise<TargetRouting> {
  const resolved = await resolveTargetPrefix(env, userId, index, target)
  if (!resolved) return { usable: false, reason: "unresolved", unusable_until: null }

  const candidates = await candidatesForTarget(env, userId, resolved)
  if (candidates.length === 0) return { usable: false, reason: "no_account", unusable_until: null }

  const facts = await candidateFactsList(env, userId, candidates)
  if (facts.some((fact) => fact.usable)) return { usable: true, reason: null, unusable_until: null }

  const untilMs = earliestUnusableUntil(facts)
  const blockingFact = facts.find((fact) => fact.unusableUntil === untilMs)!
  return {
    usable: false,
    reason: reasonForFacts(blockingFact),
    unusable_until: untilMs === null ? null : new Date(untilMs).toISOString(),
  }
}

/** One group model's read shape: name, enriched targets, per-model routing. */
async function toModelItem(
  env: Env,
  userId: string,
  name: string,
  targets: GroupTarget[],
): Promise<Record<string, unknown>> {
  const [enriched, routingTargets] = await Promise.all([
    Promise.all(
      targets.map(async (t) => ({
        model: t.model,
        account_id: t.account_id,
        account_label: await accountLabel(env.DB, userId, t.account_id),
      })),
    ),
    Promise.all(targets.map((target, index) => routingForTarget(env, userId, index, target))),
  ])
  return {
    name,
    targets: enriched,
    // The current-route indicator, per model (docs/providers.md § Routing
    // module): what the ordered walk would dispatch right now, from stored
    // facts only.
    routing: {
      current_target_index: (() => {
        const index = routingTargets.findIndex((target) => target.usable)
        return index === -1 ? null : index
      })(),
      targets: routingTargets,
    },
  }
}

async function toListItem(env: Env, userId: string, row: ModelGroupRow): Promise<Record<string, unknown>> {
  const modelRows = await listModelsForGroup(env.DB, row.id)
  const models = await Promise.all(
    modelRows.map((m) => toModelItem(env, userId, m.name, parseGroupTargets(m.targets_json))),
  )
  return {
    id: row.id,
    name: row.name,
    slug: row.slug,
    models,
    // Raw column value, not run through the dispatch-time forward-compat
    // degrade (`routing/strategy.ts` normalizeStrategy) — today the two are
    // identical since `ordered` is the only writable value, but the read
    // API should still surface exactly what's stored.
    strategy: row.strategy ?? DEFAULT_STRATEGY,
    created_at: row.created_at,
    updated_at: row.updated_at,
  }
}

modelGroupRoutes.get("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const rows = await listModelGroups(c.env.DB, user.id)
  const groups = await Promise.all(rows.map((r) => toListItem(c.env, user.id, r)))
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

  const slug = typeof body.slug === "string" ? body.slug.trim() : ""
  const slugErr = validateGroupSlug(slug)
  if (slugErr) return c.json({ error: slugErr }, 400)

  const resolvePrefix = await prefixResolver(c.env.DB, user.id)
  const resolveAccount = accountResolver(c.env.DB, user.id)
  const modelsRes = await validateGroupModels(body.models, resolvePrefix, resolveAccount)
  if (!modelsRes.ok) return c.json({ error: modelsRes.error }, 400)

  // `strategy` defaults to `ordered`; only `ordered` is accepted today
  // (docs/providers.md § Routing module).
  const strategy = body.strategy === undefined ? DEFAULT_STRATEGY : body.strategy
  const strategyErr = validateStrategy(strategy)
  if (strategyErr) return c.json({ error: strategyErr }, 400)

  const count = await countModelGroups(c.env.DB, user.id)
  if (count >= MAX_MODEL_GROUPS_PER_USER) {
    return c.json({ error: `maximum of ${MAX_MODEL_GROUPS_PER_USER} model groups reached` }, 400)
  }

  const existing = await listModelGroups(c.env.DB, user.id)
  if (existing.some((g) => g.name === name)) {
    return c.json({ error: `a model group named "${name}" already exists` }, 400)
  }
  if (existing.some((g) => g.slug === slug)) {
    return c.json({ error: `slug "${slug}" is already used by another of your groups` }, 400)
  }

  const row = await insertModelGroup(c.env.DB, {
    userId: user.id,
    name,
    slug,
    strategy: strategy as string,
  })
  await replaceGroupModels(c.env.DB, { userId: user.id, groupId: row.id, models: modelsRes.models })
  return c.json(await toListItem(c.env, user.id, row), 201)
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

  let slug: string | undefined
  if (body.slug !== undefined) {
    slug = typeof body.slug === "string" ? body.slug.trim() : ""
    const err = validateGroupSlug(slug)
    if (err) return c.json({ error: err }, 400)
    const rows = await listModelGroups(c.env.DB, user.id)
    if (rows.some((g) => g.id !== id && g.slug === slug)) {
      return c.json({ error: `slug "${slug}" is already used by another of your groups` }, 400)
    }
  }

  let models: GroupModelInput[] | undefined
  if (body.models !== undefined) {
    const resolvePrefix = await prefixResolver(c.env.DB, user.id)
    const resolveAccount = accountResolver(c.env.DB, user.id)
    const res = await validateGroupModels(body.models, resolvePrefix, resolveAccount)
    if (!res.ok) return c.json({ error: res.error }, 400)
    models = res.models
  }

  let strategy: string | undefined
  if (body.strategy !== undefined) {
    const err = validateStrategy(body.strategy)
    if (err) return c.json({ error: err }, 400)
    strategy = body.strategy as string
  }

  await updateModelGroupFields(c.env.DB, id, { name, slug, strategy })
  if (models) {
    await replaceGroupModels(c.env.DB, { userId: user.id, groupId: id, models })
  }
  const updated = await getModelGroupById(c.env.DB, user.id, id)
  return c.json(await toListItem(c.env, user.id, updated ?? existing))
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
