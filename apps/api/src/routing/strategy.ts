/**
 * Strategies (docs/providers.md § Routing module "Strategies"). A strategy
 * does exactly one thing: order the candidates. Configured per group
 * (`model_groups.strategy`) and per provider pool (`provider_settings`),
 * both default `ordered` — the only value accepted today.
 */
import type { CandidateFacts, OrderedCandidate, RoutingCandidate, StrategyContext } from "./types"

export const DEFAULT_STRATEGY = "ordered"

/**
 * Reads of an unrecognized stored strategy value degrade to `ordered`
 * (docs/database.md `model_groups.strategy` / `provider_settings.strategy`)
 * — forward compat for a future value this deploy predates.
 */
export function normalizeStrategy(value: string | null | undefined): string {
  return value === DEFAULT_STRATEGY ? value : DEFAULT_STRATEGY
}

/**
 * `ordered`: keep candidate order — target index, then pool priority.
 * Exactly the pre-refactor behavior, since `candidates.ts` already builds
 * its list in that order. Every future strategy (usage-balanced,
 * spend-aware) is another implementation of this same narrow interface,
 * selected by `ctx.strategy` — dispatch never changes.
 */
export function orderCandidates(
  candidates: RoutingCandidate[],
  facts: CandidateFacts[],
  ctx: StrategyContext,
): OrderedCandidate[] {
  const paired = candidates.map((candidate, i) => ({ candidate, facts: facts[i]! }))
  // `ctx.strategy` is read here (not just accepted) so a future non-ordered
  // implementation has one obvious branch point to land in.
  switch (normalizeStrategy(ctx.strategy)) {
    case "ordered":
    default:
      return paired
  }
}
