/**
 * Shared types for the routing module (docs/providers.md § Routing module) —
 * the single owner of account/target selection, used by both entry shapes
 * (group alias and direct `provider/model`). See candidates.ts / facts.ts /
 * strategy.ts / feedback.ts for the four concerns this module splits into.
 */
import type { AccountRow } from "../db/accounts"
import type { CustomProviderRow } from "../db/custom_providers"
import type { ProviderAdapter } from "../providers/types"

/**
 * One `(provider, upstreamModel, account)` candidate — the flattened unit
 * dispatch walks. For a group alias, `targetIndex` is the target's array
 * position (priority) and `pinned` mirrors whether that target pinned one
 * account; for a direct call there is exactly one implicit unpinned target
 * (`targetIndex: 0`).
 */
export type RoutingCandidate = {
  targetIndex: number
  pinned: boolean
  provider: string
  upstreamModel: string
  isBuiltin: boolean
  customProvider?: CustomProviderRow
  adapter: ProviderAdapter
  account: AccountRow
}

/** Per-candidate usability, computed from stored state only (facts.ts). */
export type CandidateFacts = {
  usable: boolean
  /** Epoch-ms this candidate is known unusable until (bench and/or an exhausted usage window), or `null` when nothing currently constrains it. */
  unusableUntil: number | null
  /** Individual stored-state components behind `unusableUntil`, for route indicators. */
  benchUntil: number | null
  usageWindowUntil: number | null
}

export type OrderedCandidate = { candidate: RoutingCandidate; facts: CandidateFacts }

/** Carried through ordering even though `ordered` ignores it — future stickiness (docs/providers.md § Routing module). */
export type StrategyContext = {
  apiKeyId: string | null
  strategy: string
}
