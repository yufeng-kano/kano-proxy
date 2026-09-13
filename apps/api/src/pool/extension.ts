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
import type { UsageWindow } from "../providers/types"
import type { RoutingCandidate } from "../routing/types"

/** One other user's builtin-provider account row, offered to a viewer. */
export type SharedAccount = {
  /** The owner's `upstream_accounts` row — builtin providers only; its credential is decrypted inside dispatch, never returned by a route. */
  account: AccountRow
  /** The viewer's own ordering value; merged with the viewer's rows by `(priority DESC, created_at DESC)`. */
  priority: number
  share: { teamId: string; teamName: string; ownerLabel: string }
  /**
   * The edition's own bars for this viewer on this row — what the viewer may
   * still use of it (an allowance), never the owner's upstream windows. Only
   * filled when asked for with `{ usage: true }`; routing never asks.
   */
  usage?: { windows: UsageWindow[] } | null
}

export type ListSharedOptions = {
  /** Fill `SharedAccount.usage`; the Providers page asks, the router does not. */
  usage?: boolean
}

/** One admitted attempt, settled exactly once at the point its `request_logs` row is decided. */
export type AttemptLease = { settle(outcome: "consumed" | "released"): Promise<void> }

export interface PoolExtension {
  /** Rows the viewer may borrow for one builtin provider. Never called for pinned targets or custom/CLI providers. */
  listShared(env: Env, viewerUserId: string, provider: ProviderId, options?: ListSharedOptions): Promise<SharedAccount[]>
  /** Every candidate attempt, own or shared. `null` = not governed; `{ skip: true }` = exhausted, move on; a lease = admitted. */
  reserveAttempt(
    env: Env,
    ctx: { userId: string; apiKeyId: string | null; upstreamModel: string },
    candidate: RoutingCandidate,
  ): Promise<AttemptLease | { skip: true } | null>
  /** Reorder a shared row inside the viewer's merged pool; `false` when the viewer may not (404 to the caller). */
  setSharedPriority(env: Env, viewerUserId: string, accountId: string, priority: number): Promise<boolean>
  /**
   * Extra bars for the viewer's OWN rows of one provider — e.g. the viewer's
   * allowance on an account they lend out and are themselves limited on.
   * Appended after the row's upstream windows on the Providers page only;
   * they never feed routing facts.
   */
  ownBars?(env: Env, viewerUserId: string, provider: ProviderId, accountIds: string[]): Promise<Map<string, UsageWindow[]>>
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

/**
 * A borrowed row's upstream response headers, minus everything that describes
 * the **owner's** account rather than this request (docs/cloud-edition.md
 * § "Pool extension"): rate-limit budgets and reset times, `retry-after`, and
 * any organization/account identifier the upstream echoes back. Those are
 * another user's quota state — a borrower that honors them throttles itself
 * on limits it does not have, and they name the sharer.
 *
 * Only ever called for a candidate carrying `share`; own rows and standalone
 * installs keep the byte-identical passthrough they always had.
 */
export function borrowerSafeHeaders(headers: Headers): Headers {
  const out = new Headers()
  for (const [name, value] of headers.entries()) {
    const key = name.toLowerCase()
    if (
      // `anthropic-ratelimit-*`, `x-ratelimit-*`, `ratelimit-*`, and the
      // proxy-internal `x-kano-ratelimit-reset` hint (docs/providers.md § Penalties).
      key.includes("ratelimit") ||
      key.includes("rate-limit") ||
      key === "retry-after" ||
      // Owner identity: `anthropic-organization-id`, `openai-organization`,
      // `x-org-id`, `anthropic-account-id`, …
      key.includes("organization") ||
      key.includes("-org-id") ||
      key.includes("account-id")
    ) {
      continue
    }
    out.set(name, value)
  }
  return out
}
