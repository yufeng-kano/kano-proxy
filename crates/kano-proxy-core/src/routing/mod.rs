//! Candidate selection, facts, strategy and feedback (apps/api/src/routing, docs/providers.md § Routing module).

pub mod candidates;
pub mod facts;
pub mod feedback;
pub mod strategy;
pub mod types;

pub use types::{CandidateFacts, OrderedCandidate, RoutingCandidate, StrategyContext};
