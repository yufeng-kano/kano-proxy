//! Candidate selection, facts, strategy and feedback (docs/providers.md § Routing module).

pub mod candidates;
pub mod facts;
pub mod feedback;
pub mod strategy;
pub mod types;

pub use types::{CandidateFacts, OrderedCandidate, RoutingCandidate, StrategyContext};
