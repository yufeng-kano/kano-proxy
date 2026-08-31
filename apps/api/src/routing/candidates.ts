/**
 * Candidate expansion (docs/providers.md § Routing module "Candidates").
 * Turns a resolved request — a direct `provider/model`, or a group
 * endpoint's model — into one flat, ordered `RoutingCandidate[]`:
 *
 * - A **direct** call is one implicit unpinned target: that pool's accounts
 *   in priority order.
 * - A **group model** expands each target of its list in array order: a pinned target
 *   contributes exactly its one account (if it still exists); an unpinned
 *   target contributes every account of that provider's pool, in
 *   pool-priority order.
 *
 * A target whose prefix no longer resolves (e.g. a deleted custom provider)
 * contributes nothing, exactly as the old `pickGroupTarget` walk did — this
 * module owns that skip now. Usability (bench / usage-window exhaustion) is
 * NOT decided here — see facts.ts; this module only decides which
 * `(provider, upstreamModel, account)` triples exist at all.
 */
import { getAccount, listAccounts } from "../db/accounts"
import { getCliProviderBySlug } from "../db/cli"
import { getCustomProviderBySlug, type CustomProviderRow } from "../db/custom_providers"
import {
  getGroupModelByName,
  parseGroupTargets,
  type GroupTarget,
  type ModelGroupModelRow,
  type ModelGroupRow,
} from "../db/model_groups"
import { getProviderStrategy } from "../db/provider_settings"
import type { Env, ProviderId } from "../env"
import { isProviderId } from "../env"
import { createCliAdapter } from "../providers/cli"
import { createCustomAnthropicAdapter } from "../providers/custom_anthropic"
import { createCustomOpenAIAdapter } from "../providers/custom_openai"
import { getAdapter } from "../providers"
import type { ProviderAdapter } from "../providers/types"
import { splitModelId } from "../utils/model"
import type { RoutingCandidate } from "./types"

/** A target/direct-call's provider once its `model` prefix has resolved — before accounts are looked up. */
export type ResolvedTarget = {
  targetIndex: number
  provider: string
  upstreamModel: string
  isBuiltin: boolean
  customProvider?: CustomProviderRow
  adapter: ProviderAdapter
  /** Pinned `upstream_accounts` id, or `null` for an unpinned (whole-pool) target. */
  accountId: string | null
}

function adapterFor(isBuiltin: boolean, provider: string, customProvider?: CustomProviderRow): ProviderAdapter {
  if (isBuiltin) return getAdapter(provider as ProviderId)
  const row = customProvider!
  return row.format === "anthropic" ? createCustomAnthropicAdapter(row) : createCustomOpenAIAdapter(row)
}

/**
 * Split one group target's `model` string into a resolved provider/adapter,
 * scoped to `userId` — a target whose prefix no longer resolves (e.g. a
 * deleted custom provider) returns `null` and is simply omitted.
 */
export async function resolveTargetPrefix(
  env: Env,
  userId: string,
  targetIndex: number,
  target: GroupTarget,
): Promise<ResolvedTarget | null> {
  const split = splitModelId(target.model)
  if (!split) return null
  if (isProviderId(split.prefix)) {
    return {
      targetIndex,
      provider: split.prefix,
      upstreamModel: split.upstreamModel,
      isBuiltin: true,
      adapter: getAdapter(split.prefix),
      accountId: target.account_id,
    }
  }
  const row = await getCustomProviderBySlug(env.DB, userId, split.prefix)
  if (row) {
    return {
      targetIndex,
      provider: row.slug,
      upstreamModel: split.upstreamModel,
      isBuiltin: false,
      customProvider: row,
      adapter: adapterFor(false, row.slug, row),
      accountId: target.account_id,
    }
  }
  // Third prefix branch (docs/cli.md): builtin → custom slug → CLI slug.
  const cliRow = await getCliProviderBySlug(env.DB, userId, split.prefix)
  if (!cliRow) return null
  return {
    targetIndex,
    provider: cliRow.slug,
    upstreamModel: split.upstreamModel,
    isBuiltin: false,
    adapter: createCliAdapter(env, cliRow),
    accountId: target.account_id,
  }
}

/**
 * Every account of one provider's pool, in the pool's own priority order
 * (`listAccounts`' `ORDER BY priority DESC, created_at DESC`) — the
 * candidates an unpinned target (or a direct call) contributes.
 */
async function poolCandidatesFor(env: Env, userId: string, target: ResolvedTarget): Promise<RoutingCandidate[]> {
  const rows = await listAccounts(env.DB, userId, target.provider)
  return rows.map((account) => ({
    targetIndex: target.targetIndex,
    pinned: false,
    provider: target.provider,
    upstreamModel: target.upstreamModel,
    isBuiltin: target.isBuiltin,
    customProvider: target.customProvider,
    adapter: target.adapter,
    account,
  }))
}

/**
 * The one candidate a pinned target contributes — exactly that
 * `upstream_accounts` row, if it still exists and still belongs to the
 * target's provider (docs/providers.md § Model groups "Account pinning").
 * A deleted account, or one that quietly belongs to a different provider
 * than the target claims (should never happen — `provider` is immutable —
 * but checked defensively, mirroring the old `groupTargetUsability`),
 * contributes nothing: same as an empty pool for this target.
 */
async function pinnedCandidateFor(
  env: Env,
  userId: string,
  target: ResolvedTarget,
): Promise<RoutingCandidate | null> {
  const account = await getAccount(env.DB, userId, target.accountId!)
  if (!account || account.provider !== target.provider) return null
  return {
    targetIndex: target.targetIndex,
    pinned: true,
    provider: target.provider,
    upstreamModel: target.upstreamModel,
    isBuiltin: target.isBuiltin,
    customProvider: target.customProvider,
    adapter: target.adapter,
    account,
  }
}

/** Candidates for one already-resolved target, honoring pinning. */
export async function candidatesForTarget(env: Env, userId: string, target: ResolvedTarget): Promise<RoutingCandidate[]> {
  if (target.accountId) {
    const c = await pinnedCandidateFor(env, userId, target)
    return c ? [c] : []
  }
  return poolCandidatesFor(env, userId, target)
}

/**
 * One implicit target's candidates — the direct `provider/model` case, or
 * dispatch's single-pool fallback when it wasn't handed a pre-built
 * candidate list. `accountId` pins it, same as a group target
 * (docs/providers.md § Model groups "Account pinning"); omitted/`null` for
 * the ordinary whole-pool case.
 */
export async function poolCandidates(
  env: Env,
  userId: string,
  target: {
    provider: string
    upstreamModel: string
    isBuiltin: boolean
    customProvider?: CustomProviderRow
    adapter: ProviderAdapter
    accountId?: string | null
  },
): Promise<RoutingCandidate[]> {
  return candidatesForTarget(env, userId, { ...target, targetIndex: 0, accountId: target.accountId ?? null })
}

export type GroupExpansion = {
  candidates: RoutingCandidate[]
  /** The resolved targets in array order — target 0 if it resolved, else the next one that did; `[]` when nothing resolved (`invalid_model`). */
  resolvedTargets: ResolvedTarget[]
}

/**
 * Expand every target of one group model, in array order, into the flat
 * candidate list — the same failover loop now runs cross-target and in-pool
 * (docs/providers.md § Routing module).
 */
export async function groupModelCandidates(
  env: Env,
  userId: string,
  modelRow: ModelGroupModelRow,
): Promise<GroupExpansion> {
  const targets = parseGroupTargets(modelRow.targets_json)
  const resolvedTargets: ResolvedTarget[] = []
  for (let i = 0; i < targets.length; i++) {
    const resolved = await resolveTargetPrefix(env, userId, i, targets[i]!)
    if (resolved) resolvedTargets.push(resolved)
  }
  const perTarget = await Promise.all(resolvedTargets.map((t) => candidatesForTarget(env, userId, t)))
  return { candidates: perTarget.flat(), resolvedTargets }
}

/** Top-level combinator result for a raw `model` string — used by routes. */
export type RoutingResolution = {
  /** The exact client-sent string (a group endpoint's model name, or `provider/model`). */
  raw: string
  /** Set when the request came through a group endpoint: `<slug>/<model name>` (docs/database.md `request_logs.group_name`). */
  groupName?: string
  /**
   * The first resolved target's provider/adapter — shape/logging metadata
   * only (native-Anthropic-passthrough check, count_tokens rejection,
   * request_logs on the invalid/no-account paths). Actual account selection
   * across `candidates` is the routing module's job at dispatch time, not
   * this field's.
   */
  primary: {
    provider: string
    upstreamModel: string
    adapter: ProviderAdapter
    isBuiltin: boolean
    customProvider?: CustomProviderRow
  }
  candidates: RoutingCandidate[]
  /** `model_groups.strategy` (group hit) or `provider_settings.strategy` (direct call) — raw stored value, dispatch normalizes it. */
  strategy: string
}

/**
 * Shared model-id resolution for `/openai/v1` and `/anthropic`: split on the
 * first "/", try the builtin `ProviderId` union first, then a per-user
 * `custom_providers` lookup. A `model` with no "/" is `null` (`invalid_model`)
 * — since v4 nothing bare resolves on the shared bases; group models live on
 * their own endpoints and resolve via `resolveGroupModelCandidates`
 * (docs/api.md "Model routing").
 */
export async function resolveCandidates(
  env: Env,
  userId: string,
  model: string,
): Promise<RoutingResolution | null> {
  const split = splitModelId(model)
  if (!split) return null

  if (isProviderId(split.prefix)) {
    const adapter = getAdapter(split.prefix)
    const primary = {
      provider: split.prefix,
      upstreamModel: split.upstreamModel,
      adapter,
      isBuiltin: true,
    }
    return {
      raw: split.raw,
      primary,
      candidates: await poolCandidates(env, userId, primary),
      strategy: await getProviderStrategy(env.DB, userId, split.prefix),
    }
  }

  const row = await getCustomProviderBySlug(env.DB, userId, split.prefix)
  if (row) {
    const adapter = adapterFor(false, row.slug, row)
    const primary = {
      provider: row.slug,
      upstreamModel: split.upstreamModel,
      adapter,
      isBuiltin: false,
      customProvider: row,
    }
    return {
      raw: split.raw,
      primary,
      candidates: await poolCandidates(env, userId, primary),
      strategy: await getProviderStrategy(env.DB, userId, row.slug),
    }
  }

  // Third prefix branch (docs/cli.md): a CLI provider behaves like any
  // provider on the routing surface — its one internal account row rides the
  // same pool machinery, only the adapter's transport differs.
  const cliRow = await getCliProviderBySlug(env.DB, userId, split.prefix)
  if (!cliRow) return null
  const cliPrimary = {
    provider: cliRow.slug,
    upstreamModel: split.upstreamModel,
    adapter: createCliAdapter(env, cliRow),
    isBuiltin: false,
  }
  return {
    raw: split.raw,
    primary: cliPrimary,
    candidates: await poolCandidates(env, userId, cliPrimary),
    strategy: await getProviderStrategy(env.DB, userId, cliRow.slug),
  }
}

/**
 * Group-endpoint resolution (docs/api.md § Group endpoints): the route has
 * already resolved the slug to `group` (scoped to the caller — an unknown
 * slug is the route's 404, not this function's concern); the request's
 * `model` is matched exactly against that group's model names. A miss — or a
 * hit whose targets all fail to resolve — is `null` (`invalid_model`).
 */
export async function resolveGroupModelCandidates(
  env: Env,
  userId: string,
  group: ModelGroupRow,
  model: string,
): Promise<RoutingResolution | null> {
  const trimmed = model.trim()
  if (!trimmed) return null

  const modelRow = await getGroupModelByName(env.DB, group.id, trimmed)
  if (!modelRow) return null

  const { candidates, resolvedTargets } = await groupModelCandidates(env, userId, modelRow)
  // No target's prefix resolves at all (e.g. every target pointed at a
  // since-deleted custom provider) — the model behaves as invalid_model.
  if (resolvedTargets.length === 0) return null

  const first = resolvedTargets[0]!
  return {
    raw: trimmed,
    groupName: `${group.slug}/${modelRow.name}`,
    primary: {
      provider: first.provider,
      upstreamModel: first.upstreamModel,
      adapter: first.adapter,
      isBuiltin: first.isBuiltin,
      customProvider: first.customProvider,
    },
    candidates,
    strategy: group.strategy,
  }
}
