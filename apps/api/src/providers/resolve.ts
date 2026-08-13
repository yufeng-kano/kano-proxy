import { getAccount, listAccounts } from "../db/accounts"
import { getCustomProviderBySlug, type CustomProviderRow } from "../db/custom_providers"
import { getModelGroupByName, parseGroupTargets, type GroupTarget } from "../db/model_groups"
import type { Env, ProviderId } from "../env"
import { isProviderId } from "../env"
import { isBenched } from "../pool/bench"
import { splitModelId } from "../utils/model"
import { createCustomAnthropicAdapter } from "./custom_anthropic"
import { createCustomOpenAIAdapter } from "./custom_openai"
import { getAdapter } from "./index"
import type { ProviderAdapter } from "./types"

export type ResolvedModel = {
  /** Builtin `ProviderId`, or the custom provider's slug. */
  provider: string
  upstreamModel: string
  raw: string
  adapter: ProviderAdapter
  isBuiltin: boolean
  /** Present only when `isBuiltin` is false. */
  customProvider?: CustomProviderRow
  /** Present only when `model` was a model-group bare name that expanded to this target. */
  group?: { name: string }
  /**
   * Present only when the chosen group target pinned a specific
   * `upstream_accounts` row (docs/providers.md § Model groups "Account
   * pinning") — dispatch must acquire exactly this account and never fail
   * over to a sibling in the same provider's pool.
   */
  pinnedAccountId?: string
}

type ResolvableTarget = {
  provider: string
  upstreamModel: string
  isBuiltin: boolean
  customProvider?: CustomProviderRow
  /** Pinned `upstream_accounts` id from the group target, or `null` for a whole-pool (unpinned) target. */
  accountId: string | null
}

/**
 * Split one group target's `model` string into a resolvable provider/adapter,
 * scoped to `userId` the same way the top-level slash path resolves a prefix
 * — a target whose prefix no longer resolves (e.g. a deleted custom
 * provider) is simply omitted by the caller. The target's `account_id`
 * (pinning) rides along untouched; it is validated for existence/bench state
 * later, in `pickGroupTarget`.
 */
async function resolveGroupTarget(
  env: Env,
  userId: string,
  target: GroupTarget,
): Promise<ResolvableTarget | null> {
  const split = splitModelId(target.model)
  if (!split) return null
  if (isProviderId(split.prefix)) {
    return {
      provider: split.prefix,
      upstreamModel: split.upstreamModel,
      isBuiltin: true,
      accountId: target.account_id,
    }
  }
  const row = await getCustomProviderBySlug(env.DB, userId, split.prefix)
  if (!row) return null
  return {
    provider: row.slug,
    upstreamModel: split.upstreamModel,
    isBuiltin: false,
    customProvider: row,
    accountId: target.account_id,
  }
}

function adapterFor(target: ResolvableTarget): ProviderAdapter {
  if (target.isBuiltin) return getAdapter(target.provider as ProviderId)
  const row = target.customProvider!
  return row.format === "anthropic" ? createCustomAnthropicAdapter(row) : createCustomOpenAIAdapter(row)
}

/**
 * Per-target usability (docs/providers.md § Model groups "Account pinning"):
 * pinned = that exact `upstream_accounts` row exists (owned by `userId`,
 * `provider` matching the target) and is not benched; unpinned = the
 * provider's pool has ≥1 bound, non-benched account. A pinned account that
 * was deleted, or whose row now belongs to a different provider (should
 * never happen — `upstream_accounts.provider` is immutable — but checked
 * defensively the same way write-time validation does), reads as no bound
 * account at all for this target, same as an empty pool.
 */
async function groupTargetUsability(
  env: Env,
  userId: string,
  target: ResolvableTarget,
): Promise<{ hasAccounts: boolean; hasUsable: boolean }> {
  if (target.accountId) {
    const row = await getAccount(env.DB, userId, target.accountId)
    if (!row || row.provider !== target.provider) return { hasAccounts: false, hasUsable: false }
    const benched = await isBenched(env, userId, target.provider, row.id)
    return { hasAccounts: true, hasUsable: !benched }
  }
  const rows = await listAccounts(env.DB, userId, target.provider)
  let hasUsable = false
  for (const row of rows) {
    if (!(await isBenched(env, userId, target.provider, row.id))) {
      hasUsable = true
      break
    }
  }
  return { hasAccounts: rows.length > 0, hasUsable }
}

/**
 * Ordered-priority target walk (docs/providers.md § Model groups):
 * 1. First resolvable target with ≥1 bound, non-benched account wins
 *    (pinned targets check exactly their one account — see
 *    `groupTargetUsability`).
 * 2. All resolvable targets' pools benched (or empty) → first resolvable
 *    target that has any bound account (dispatched anyway, yielding the
 *    normal 503/`upstream_unavailable` path).
 * 3. No resolvable target has any bound account → the first resolvable
 *    target period (yields `no_upstream_account`).
 * `resolvedTargets` is assumed non-empty — callers check that first.
 */
async function pickGroupTarget(
  env: Env,
  userId: string,
  resolvedTargets: ResolvableTarget[],
): Promise<ResolvableTarget> {
  const info = await Promise.all(resolvedTargets.map((t) => groupTargetUsability(env, userId, t)))
  let idx = info.findIndex((i) => i.hasUsable)
  if (idx === -1) idx = info.findIndex((i) => i.hasAccounts)
  if (idx === -1) idx = 0
  return resolvedTargets[idx]!
}

/**
 * Shared model-id resolution for `/openai/v1` and `/anthropic`: split on the
 * first "/", try the builtin `ProviderId` union first, and only fall back to
 * a per-user `custom_providers` lookup when the prefix isn't a builtin — a
 * miss either way is the same `invalid_model` case to the caller. The custom
 * lookup is scoped to `userId` by construction, so a slug never resolves
 * across users. Adapters for custom providers are built fresh per call —
 * they carry the row's `base_url`/`format` and are never cached or added to
 * the static builtin registry.
 *
 * A `model` with no "/" at all is a candidate **model group** name (docs/api.md
 * "Model routing" step 1) — looked up in `model_groups` scoped to `userId` and,
 * on a hit, expanded to the first usable target per the ordered-priority walk
 * in docs/providers.md § Model groups. `raw` on a group hit stays the group
 * name itself (the exact client-sent string) — never the expanded target —
 * so response echo keeps showing the client what it sent.
 */
export async function resolveModel(
  env: Env,
  userId: string,
  model: string,
): Promise<ResolvedModel | null> {
  const split = splitModelId(model)
  if (!split) return resolveGroupModel(env, userId, model)

  if (isProviderId(split.prefix)) {
    return {
      provider: split.prefix,
      upstreamModel: split.upstreamModel,
      raw: split.raw,
      adapter: getAdapter(split.prefix),
      isBuiltin: true,
    }
  }

  const row = await getCustomProviderBySlug(env.DB, userId, split.prefix)
  if (!row) return null
  const adapter =
    row.format === "anthropic" ? createCustomAnthropicAdapter(row) : createCustomOpenAIAdapter(row)
  return {
    provider: row.slug,
    upstreamModel: split.upstreamModel,
    raw: split.raw,
    adapter,
    isBuiltin: false,
    customProvider: row,
  }
}

async function resolveGroupModel(
  env: Env,
  userId: string,
  model: string,
): Promise<ResolvedModel | null> {
  const trimmed = model.trim()
  if (!trimmed) return null

  const group = await getModelGroupByName(env.DB, userId, trimmed)
  if (!group) return null

  const targets = parseGroupTargets(group.targets_json)
  const resolvedTargets: ResolvableTarget[] = []
  for (const target of targets) {
    const resolved = await resolveGroupTarget(env, userId, target)
    if (resolved) resolvedTargets.push(resolved)
  }
  // No target's prefix resolves at all (e.g. every target pointed at a
  // since-deleted custom provider) — the group behaves as invalid_model.
  if (resolvedTargets.length === 0) return null

  const chosen = await pickGroupTarget(env, userId, resolvedTargets)
  return {
    provider: chosen.provider,
    upstreamModel: chosen.upstreamModel,
    raw: trimmed,
    adapter: adapterFor(chosen),
    isBuiltin: chosen.isBuiltin,
    customProvider: chosen.customProvider,
    group: { name: group.name },
    pinnedAccountId: chosen.accountId ?? undefined,
  }
}
