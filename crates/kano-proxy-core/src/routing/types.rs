//! Shared routing types (apps/api/src/routing/types.ts).

use crate::db::AccountRow;
use crate::pool::extension::ShareInfo;
use crate::providers::DynAdapter;

/// One `(provider, upstreamModel, account)` candidate — the flattened unit dispatch walks.
#[derive(Clone)]
pub struct RoutingCandidate {
    pub target_index: usize,
    pub pinned: bool,
    pub provider: String,
    pub upstream_model: String,
    pub is_builtin: bool,
    pub custom_provider: Option<crate::db::CustomProviderRow>,
    pub adapter: DynAdapter,
    pub account: AccountRow,
    /// Set only when the row came from another user through the pool extension.
    pub share: Option<ShareInfo>,
}

/// Per-candidate usability, computed from stored state only (facts.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateFacts {
    pub usable: bool,
    /// Epoch-ms this candidate is known unusable until, or `None` when nothing constrains it.
    pub unusable_until: Option<i64>,
    pub bench_until: Option<i64>,
    pub usage_window_until: Option<i64>,
}

pub struct OrderedCandidate {
    pub candidate: RoutingCandidate,
    pub facts: CandidateFacts,
}

/// Carried through ordering even though `ordered` ignores it — future stickiness.
#[derive(Debug, Clone)]
pub struct StrategyContext {
    pub api_key_id: Option<String>,
    pub strategy: String,
}
