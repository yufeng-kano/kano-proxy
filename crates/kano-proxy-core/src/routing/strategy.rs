//! Port of apps/api/src/routing/strategy.ts (docs/providers.md § Routing module "Strategies").
//!
//! A strategy does exactly one thing: order the candidates. Configured per group
//! (`model_groups.strategy`) and per provider pool (`provider_settings`), both default
//! `ordered` — the only value accepted today.

use crate::routing::types::{CandidateFacts, OrderedCandidate, RoutingCandidate, StrategyContext};

pub const DEFAULT_STRATEGY: &str = "ordered";

/// A stored value this deploy does not recognize degrades to `ordered`
/// (docs/database.md `model_groups.strategy` / `provider_settings.strategy`) — forward compat
/// for a future value this deploy predates.
pub fn normalize_strategy(value: Option<&str>) -> String {
    match value {
        Some(v) if v == DEFAULT_STRATEGY => v.to_string(),
        _ => DEFAULT_STRATEGY.to_string(),
    }
}

/// `ordered`: keep candidate order — target index, then pool priority. Exactly the
/// pre-refactor behavior, since `candidates.rs` already builds its list in that order.
///
/// `ctx.strategy` is normalized here (not just accepted) so a future non-ordered
/// implementation — usage-balanced, spend-aware — has one obvious place to branch in; dispatch
/// never changes.
pub fn order_candidates(
    candidates: Vec<RoutingCandidate>,
    facts: &[CandidateFacts],
    ctx: &StrategyContext,
) -> Vec<OrderedCandidate> {
    let strategy = normalize_strategy(Some(&ctx.strategy));
    debug_assert_eq!(strategy, DEFAULT_STRATEGY, "ordered is the only strategy this deploy implements");
    candidates
        .into_iter()
        .zip(facts.iter().copied())
        .map(|(candidate, facts)| OrderedCandidate { candidate, facts })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::candidates::tests_support::{candidate_with, usable, unusable};

    #[test]
    fn unknown_values_degrade_to_ordered() {
        assert_eq!(normalize_strategy(Some("ordered")), "ordered");
        assert_eq!(normalize_strategy(Some("usage-balanced")), "ordered");
        assert_eq!(normalize_strategy(Some("")), "ordered");
        assert_eq!(normalize_strategy(None), "ordered");
    }

    #[test]
    fn ordered_keeps_the_candidate_order_and_pairs_each_candidates_own_facts() {
        let candidates = vec![candidate_with("acc_1", "grok", 0), candidate_with("acc_2", "grok", 1)];
        let facts = vec![usable(), unusable(10)];
        let ctx = StrategyContext { api_key_id: Some("key_1".into()), strategy: "ordered".into() };
        let ordered = order_candidates(candidates, &facts, &ctx);
        assert_eq!(ordered.iter().map(|o| o.candidate.account.id.clone()).collect::<Vec<_>>(), ["acc_1", "acc_2"]);
        assert!(ordered[0].facts.usable);
        assert_eq!(ordered[1].facts.unusable_until, Some(10));
    }

    #[test]
    fn an_unrecognized_stored_strategy_still_orders() {
        let candidates = vec![candidate_with("acc_1", "grok", 0)];
        let ctx = StrategyContext { api_key_id: None, strategy: "spend-aware".into() };
        assert_eq!(order_candidates(candidates, &[usable()], &ctx).len(), 1);
    }
}
