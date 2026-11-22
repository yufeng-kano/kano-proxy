//! Routing strategies (docs/providers.md § Routing module "Strategies").
//!
//! A strategy does exactly one thing: order the candidates. Configured per group
//! (`model_groups.strategy`) and per provider pool (`provider_settings`), both default
//! `ordered` — the only value accepted today, so dispatch does not read the stored value.

use crate::routing::types::{CandidateFacts, OrderedCandidate, RoutingCandidate};

pub const DEFAULT_STRATEGY: &str = "ordered";

/// `ordered`: keep candidate order — target index, then pool priority, the order
/// `candidates.rs` already builds its list in.
pub fn order_candidates(candidates: Vec<RoutingCandidate>, facts: &[CandidateFacts]) -> Vec<OrderedCandidate> {
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
    fn ordered_keeps_the_candidate_order_and_pairs_each_candidates_own_facts() {
        let candidates = vec![candidate_with("acc_1", "grok"), candidate_with("acc_2", "grok")];
        let facts = vec![usable(), unusable(10)];
        let ordered = order_candidates(candidates, &facts);
        assert_eq!(ordered.iter().map(|o| o.candidate.account.id.clone()).collect::<Vec<_>>(), ["acc_1", "acc_2"]);
        assert!(ordered[0].facts.usable);
        assert_eq!(ordered[1].facts.unusable_until, Some(10));
    }
}
