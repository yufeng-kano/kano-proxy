/**
 * Pool extension (docs/cloud-edition.md § "Pool extension") — the only
 * sanctioned cross-user path in the core. An edition passes one at
 * composition time (`ApplicationOptions.poolExtension`); standalone installs
 * pass none and every code path below behaves exactly as it did before.
 *
 * The core never looks up another user's rows on its own: it asks
 * `listShared` for the rows a viewer may borrow, `reserveAttempt` whether one
 * attempt may run, and `setSharedPriority` to reorder a borrowed row inside
 * the viewer's own pool. Teams — membership, limits, ledgers, UI — stay in
 * the private edition.
 */
import type { AccountRow } from "../db/accounts"
import type { Env, ProviderId } from "../env"
import type { RoutingCandidate } from "../routing/types"

/** One other user's builtin-provider account row, offered to a viewer. */
export type SharedAccount = {
  /** The owner's `upstream_accounts` row — builtin providers only; its credential is decrypted inside dispatch, never returned by a route. */
  account: AccountRow
  /** The viewer's own ordering value; merged with the viewer's rows by `(priority DESC, created_at DESC)`. */
  priority: number
  share: { teamId: string; teamName: string; ownerLabel: string }
}

/** One admitted attempt, settled exactly once at the point its `request_logs` row is decided. */
export type AttemptLease = { settle(outcome: "consumed" | "released"): Promise<void> }

export interface PoolExtension {
  /** Rows the viewer may borrow for one builtin provider. Never called for pinned targets or custom/CLI providers. */
  listShared(env: Env, viewerUserId: string, provider: ProviderId): Promise<SharedAccount[]>
  /** Every candidate attempt, own or shared. `null` = not governed; `{ skip: true }` = exhausted, move on; a lease = admitted. */
  reserveAttempt(
    env: Env,
    ctx: { userId: string; apiKeyId: string | null; upstreamModel: string },
    candidate: RoutingCandidate,
  ): Promise<AttemptLease | { skip: true } | null>
  /** Reorder a shared row inside the viewer's merged pool; `false` when the viewer may not (404 to the caller). */
  setSharedPriority(env: Env, viewerUserId: string, accountId: string, priority: number): Promise<boolean>
}

/** A lease must never take the response down with it — settle failures are the extension's problem, not the client's. */
export async function settleLease(
  lease: AttemptLease | null | undefined,
  outcome: "consumed" | "released",
): Promise<void> {
  if (!lease) return
  try {
    await lease.settle(outcome)
  } catch (error) {
    console.error("Failed to settle pool attempt lease", {
      outcome,
      error: error instanceof Error ? error.message : String(error),
    })
  }
}
