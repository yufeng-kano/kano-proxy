//! CLI agent tunnel: wire protocol, mux and the in-process registry replacing the AgentTunnel Durable Object (apps/api/src/do, docs/cli.md).

pub mod protocol;
pub mod mux;
pub mod registry;
pub mod ws;
