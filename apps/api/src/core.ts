/** Supported composition entry point for independently deployed editions. */
export { createApplication, createWorker, type ApplicationOptions } from "./application"
export { AgentTunnel } from "./do/agent_tunnel"
export type { Env, ProviderId } from "./env"
export type { HonoEnv, AppVariables } from "./auth/session"
/** Pool extension contract (docs/cloud-edition.md § "Pool extension") — the only sanctioned cross-user path. */
export type { AttemptLease, PoolExtension, ListSharedOptions, SharedAccount } from "./pool/extension"
export type { AccountRow } from "./db/accounts"
export type { RoutingCandidate } from "./routing/types"
