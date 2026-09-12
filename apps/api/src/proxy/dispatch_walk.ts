/**
 * The one candidate walk every dispatch surface shares
 * (docs/project-structure.md § Boundaries, docs/providers.md § Routing module).
 *
 * Plans the candidate list, then for each candidate: acquire (decrypt +
 * refresh), fetch with the first-byte timeout and a single 529 retry, ask
 * `routing/feedback` whether the response is a bench-type failure, and
 * either move on or hand the response back. It never shapes the client
 * response and never branches on protocol — the transports in
 * `dispatch.ts` turn its `WalkOutcome` into HTTP or SSE frames.
 */
import { decryptJson } from "../crypto/token_crypto"
import { recordEdgeTimeoutStrike } from "../db/accounts"
import type { CustomProviderRow } from "../db/custom_providers"
import type { Env, ProviderId } from "../env"
import { markBenched } from "../pool/bench"
import type { AcquiredAccount, StoredCredential } from "../pool/acquire"
import { settleLease, type AttemptLease, type PoolExtension } from "../pool/extension"
import { getAdapter } from "../providers"
import { refreshAccountUsageInBackground } from "../providers/usage_refresh"
import type { ProviderAdapter } from "../providers/types"
import { poolCandidates } from "../routing/candidates"
import { candidateFactsList, earliestUnusableUntil } from "../routing/facts"
import {
  EDGE_TIMEOUT_COOLDOWN_MS,
  agentFaultVerdict,
  isEdgeTimeoutStatus,
  penaltyForOutcome,
} from "../routing/feedback"
import { normalizeStrategy, orderCandidates } from "../routing/strategy"
import type { RoutingCandidate } from "../routing/types"

/** Keeps a Worker invocation alive for a deferred `logRequest` past the returned Response — `c.executionCtx.waitUntil` in production, a test double in tests. */
export type WaitUntil = (promise: Promise<unknown>) => void

/** The candidate walk never retries more than this many real upstream calls in one request — same cap as the old `acquireAccount`+exclude loop. */
const MAX_ATTEMPTS = 8

/**
 * The routing module's candidate list, ready for dispatch to walk
 * (docs/providers.md § Routing module). `candidates` and `strategy` let a
 * caller (a group dispatch, via the cross-target flattened list) hand in an
 * already-built, possibly cross-provider list; when omitted, this builds
 * the ordinary single-pool list dispatch has always used — the shape every
 * existing single-provider call site (including tests) still gets.
 */
export type CandidateSource = {
  userId: string
  apiKeyId: string | null
  provider: string
  adapter?: ProviderAdapter
  pinnedAccountId?: string
  /** Pre-built cross-target candidate list (group dispatch) — bypasses the single-pool builder below entirely. */
  candidates?: RoutingCandidate[]
  /** `model_groups.strategy` (candidates given) or `provider_settings.strategy` (single pool); defaults to `ordered`. */
  strategy?: string
  isBuiltin?: boolean
  customProvider?: CustomProviderRow
  /** Composition-time cross-user pool sharing (docs/cloud-edition.md § "Pool extension"); absent for standalone. */
  poolExtension?: PoolExtension
}

type Plan =
  | { kind: "no_account" }
  | { kind: "unavailable"; untilMs: number | null }
  | { kind: "usable"; ordered: RoutingCandidate[] }

async function planCandidates(env: Env, src: CandidateSource, upstreamModel: string): Promise<Plan> {
  const candidates =
    src.candidates ??
    (await poolCandidates(
      env,
      src.userId,
      {
        provider: src.provider,
        upstreamModel,
        isBuiltin: src.isBuiltin ?? true,
        customProvider: src.customProvider,
        adapter: src.adapter ?? getAdapter(src.provider as ProviderId),
        accountId: src.pinnedAccountId ?? null,
      },
      src.poolExtension,
    ))
  if (candidates.length === 0) return { kind: "no_account" }
  const facts = await candidateFactsList(env, src.userId, candidates)
  const ordered = orderCandidates(candidates, facts, {
    apiKeyId: src.apiKeyId,
    strategy: normalizeStrategy(src.strategy),
  })
  const usable = ordered.filter((o) => o.facts.usable).map((o) => o.candidate)
  if (usable.length === 0) return { kind: "unavailable", untilMs: earliestUnusableUntil(facts) }
  return { kind: "usable", ordered: usable }
}

/** Decrypt + `refreshIfNeeded` for one candidate — `null` on an unreadable credential (skipped, never counted as an attempt, mirrors the old `acquireAccount`). */
async function acquireCandidate(env: Env, candidate: RoutingCandidate): Promise<AcquiredAccount | null> {
  try {
    const credential = await decryptJson<StoredCredential>(env.TOKEN_ENCRYPTION_KEY, candidate.account.encrypted_payload)
    let acquired: AcquiredAccount = { row: candidate.account, credential }
    if (candidate.adapter.refreshIfNeeded) acquired = await candidate.adapter.refreshIfNeeded(env, acquired)
    return acquired
  } catch {
    return null
  }
}

/** Per-attempt upstream response-header deadline (docs/api.md "Keepalive and idle timeout"). */
const DEFAULT_FIRST_BYTE_TIMEOUT_MS = 180_000

/** Invalid, absent, or non-positive environment values use the documented default. */
function firstByteTimeoutMs(env: Env): number {
  const parsed = Number(env.UPSTREAM_FIRST_BYTE_TIMEOUT_MS)
  return Number.isFinite(parsed) && parsed > 0 ? Math.floor(parsed) : DEFAULT_FIRST_BYTE_TIMEOUT_MS
}

type TimedUpstreamResponse = { response: Response; timedOut: false } | { response: null; timedOut: true }

/** Wait only for upstream response headers, and always release the timer once fetch resolves or rejects. */
async function fetchUpstreamResponse(
  env: Env,
  attempt: (signal: AbortSignal) => Promise<Response>,
): Promise<TimedUpstreamResponse> {
  const controller = new AbortController()
  let timedOut = false
  const timer = setTimeout(() => {
    timedOut = true
    controller.abort()
  }, firstByteTimeoutMs(env))
  try {
    return { response: await attempt(controller.signal), timedOut: false }
  } catch (error) {
    if (timedOut) return { response: null, timedOut: true }
    throw error
  } finally {
    clearTimeout(timer)
  }
}

function waitForOverloadRetry(): Promise<void> {
  // Jitter keeps concurrent overloaded calls from immediately synchronizing;
  // zero is intentionally allowed in unit tests that stub the retry response.
  return new Promise((resolve) => setTimeout(resolve, 900 + Math.floor(Math.random() * 200)))
}

/** A 529 is transient fleet overload, so retry this account exactly once without benching it. */
async function retry529Once(call: () => Promise<TimedUpstreamResponse>): Promise<TimedUpstreamResponse> {
  const result = await call()
  if (result.timedOut || result.response.status !== 529) return result
  await cancelBody(result.response)
  await waitForOverloadRetry()
  return call()
}

async function cancelBody(res: Response | null): Promise<void> {
  try {
    await res?.body?.cancel()
  } catch {
    /* an abandoned upstream body has nothing left to tell us */
  }
}

/**
 * Bench persistence is feedback, never a reason to abort an in-flight
 * failover walk. Bench writes are scoped to the row's **owner**
 * (`account.user_id`), which is the caller for every own row and the sharer
 * for a borrowed one (docs/cloud-edition.md § "Pool extension") — a shared
 * account that just failed must really get benched, not silently no-op.
 */
async function persistBench(
  env: Env,
  candidate: RoutingCandidate,
  cooldownMs: number,
  upstreamStatus: number,
): Promise<void> {
  try {
    await markBenched(
      env,
      candidate.account.user_id,
      candidate.provider,
      candidate.account.id,
      cooldownMs,
      String(upstreamStatus),
    )
  } catch (error) {
    console.error("Failed to persist account bench", {
      accountId: candidate.account.id,
      provider: candidate.provider,
      error: error instanceof Error ? error.message : String(error),
    })
  }
}

/**
 * Edge-timeout feedback is non-fatal just like bench persistence. Every edge
 * status remains excluded from this walk; only the atomic third strike earns a
 * bench. Returning false therefore covers strikes 1–2 and a failed write.
 */
async function persistEdgeTimeoutStrike(env: Env, candidate: RoutingCandidate): Promise<boolean> {
  try {
    return await recordEdgeTimeoutStrike(env.DB, candidate.account.user_id, candidate.provider, candidate.account.id)
  } catch (error) {
    console.error("Failed to persist edge-timeout strike", {
      accountId: candidate.account.id,
      provider: candidate.provider,
      error: error instanceof Error ? error.message : String(error),
    })
    return false
  }
}

/** Apply feedback and decide whether this pre-stream response must fail over. */
async function shouldFailOverForResponse(
  env: Env,
  candidate: RoutingCandidate,
  response: Response,
): Promise<boolean> {
  // Agent-tunnel fault (docs/cli.md § Failover semantics): infrastructure
  // failure between the DO and the CLI — always the next candidate; only
  // `offline` benches (60s), and reconnect clears that bench.
  const agentFault = agentFaultVerdict(response.headers)
  if (agentFault) {
    if (agentFault.benchMs !== null) {
      try {
        await markBenched(
          env,
          candidate.account.user_id,
          candidate.provider,
          candidate.account.id,
          agentFault.benchMs,
          agentFault.reason,
        )
      } catch (error) {
        console.error("Failed to persist agent-fault bench", {
          accountId: candidate.account.id,
          provider: candidate.provider,
          error: error instanceof Error ? error.message : String(error),
        })
      }
    }
    return true
  }
  if (isEdgeTimeoutStatus(response.status)) {
    if (await persistEdgeTimeoutStrike(env, candidate)) {
      await persistBench(env, candidate, EDGE_TIMEOUT_COOLDOWN_MS, response.status)
    }
    return true
  }
  const penalty = penaltyForOutcome(response.status, response.headers, candidate.account)
  if (!penalty) return false
  await persistBench(env, candidate, penalty.cooldownMs, response.status)
  return true
}

/**
 * Recomputes the earliest bench/limit expiry across exactly the candidates
 * this walk just tried and benched — same "exclude set" the old loop used
 * for its bottom-of-loop `Retry-After`, expressed as a fresh facts read
 * (cheap: bounded by `MAX_ATTEMPTS`, and correct even though a 429's
 * cooldown can vary per candidate, unlike the old flat 300s-for-everything
 * assumption baked into `earliestBenchExpiry`).
 */
export async function recomputeUnavailableUntil(
  env: Env,
  userId: string,
  tried: RoutingCandidate[],
): Promise<number | null> {
  if (tried.length === 0) return null
  const facts = await candidateFactsList(env, userId, tried)
  return earliestUnusableUntil(facts)
}

/** One candidate's upstream call, already bound to the request body/headers; the walk supplies the acquired credential and the first-byte abort signal. */
export type UpstreamCall = (acquired: AcquiredAccount, signal: AbortSignal) => Promise<Response>

export type WalkOpts = {
  source: CandidateSource
  /** Bare upstream model id used to build the single-pool candidate list when `source.candidates` is absent. */
  upstreamModel: string
  /** Picks the adapter method for one candidate; `null` skips a candidate whose adapter lacks the endpoint (never counted as an attempt). */
  callFor: (candidate: RoutingCandidate) => UpstreamCall | null
  waitUntil: WaitUntil
  /** Eager transport: the client already went away — stop before the next acquire/fetch and drop any response already in hand. */
  cancelled?: () => boolean
  /** Filled in as the walk goes so a log written after an early exit (cancel, throw) still names the last candidate tried. */
  progress: WalkProgress
}

/** The attempt record: what the transports need to log, no matter how the walk ended. */
export type WalkProgress = {
  /** Last candidate a real upstream call was made for (`null` before the first attempt). */
  candidate: RoutingCandidate | null
  /** HTTP status of the last upstream response seen, bench-type or not. */
  upstreamStatus: number | null
}

export type WalkOutcome =
  /** Zero candidates for this target — retrying can never help (400 / `no_upstream_account`). */
  | { kind: "no_account" }
  /** Candidates exist but every one is benched or over its usage window right now (503 / `upstream_unavailable`, `Retry-After` from the plan). */
  | { kind: "unavailable"; untilMs: number | null }
  /** `cancelled()` fired mid-walk; any response in hand has been cancelled. Never returned when `cancelled` is absent. */
  | { kind: "cancelled" }
  /** The adapter call rejected (network, adapter bug) — terminal, no further candidates (502 / `upstream_error`). */
  | { kind: "fetch_error"; candidate: RoutingCandidate }
  /**
   * Every attempt was a bench-type failure: either the list ran dry
   * (`lastBenched` = the candidate whose bench response ended it) or the
   * attempt cap hit / the rest was undecryptable (`lastBenched` null). Same
   * synthesized 503 either way; `tried` is the exclude set for `Retry-After`.
   */
  | { kind: "exhausted"; tried: RoutingCandidate[]; lastBenched: RoutingCandidate | null }
  /**
   * A non-bench response — success or a terminal upstream error — for the
   * transport to deliver. `lease` is this attempt's still-open pool-extension
   * lease (docs/cloud-edition.md § "Pool extension"): the transport settles
   * it exactly once, where it decides the attempt's `request_logs` row. Every
   * other outcome above has already settled its own leases `released`.
   */
  | { kind: "response"; candidate: RoutingCandidate; response: Response; lease: AttemptLease | null }

export async function walkCandidates(env: Env, opts: WalkOpts): Promise<WalkOutcome> {
  const cancelled = opts.cancelled ?? (() => false)
  const ext = opts.source.poolExtension
  const plan = await planCandidates(env, opts.source, opts.upstreamModel)
  if (cancelled()) return { kind: "cancelled" }
  if (plan.kind === "no_account") return { kind: "no_account" }
  if (plan.kind === "unavailable") return { kind: "unavailable", untilMs: plan.untilMs }

  let lastResponse: Response | null = null
  let lastCandidate: RoutingCandidate | null = null
  let idx = 0
  let attempts = 0
  let ranOutOfCandidates = false
  for (; attempts < MAX_ATTEMPTS; ) {
    if (cancelled()) {
      await cancelBody(lastResponse)
      return { kind: "cancelled" }
    }
    const candidate = plan.ordered[idx]
    if (!candidate) {
      ranOutOfCandidates = true
      break
    }
    idx++
    const call = opts.callFor(candidate)
    if (!call) continue

    // Pool extension (docs/cloud-edition.md § "Pool extension"): every
    // attempt, own row or shared, is offered for admission immediately
    // before the acquire. A `skip` is exhaustion of that row's governed
    // budget — move on without spending one of MAX_ATTEMPTS on it.
    let lease: AttemptLease | null = null
    if (ext) {
      const reserved = await ext.reserveAttempt(
        env,
        { userId: opts.source.userId, apiKeyId: opts.source.apiKeyId, upstreamModel: candidate.upstreamModel },
        candidate,
      )
      if (reserved && "skip" in reserved) continue
      lease = reserved
    }

    const acquired = await acquireCandidate(env, candidate)
    if (cancelled()) {
      await settleLease(lease, "released")
      await cancelBody(lastResponse)
      return { kind: "cancelled" }
    }
    if (!acquired) {
      await settleLease(lease, "released")
      continue
    }
    attempts++
    opts.progress.candidate = candidate

    let fetched: TimedUpstreamResponse
    try {
      fetched = await retry529Once(() => fetchUpstreamResponse(env, (signal) => call(acquired, signal)))
    } catch {
      // Terminal for this request and logged as `upstream_error` with no
      // output, so the attempt is released, not consumed.
      await settleLease(lease, "released")
      await cancelBody(lastResponse)
      return { kind: "fetch_error", candidate }
    }
    if (fetched.timedOut) {
      await settleLease(lease, "released")
      continue
    }
    const res = fetched.response
    opts.progress.upstreamStatus = res.status
    opts.waitUntil(refreshAccountUsageInBackground(env, candidate.account, candidate.adapter))
    if (cancelled()) {
      await settleLease(lease, "released")
      await cancelBody(lastResponse)
      await cancelBody(res)
      return { kind: "cancelled" }
    }

    if (await shouldFailOverForResponse(env, candidate, res)) {
      // Benched / edge-timed-out / retried: this attempt produced nothing.
      await settleLease(lease, "released")
      await cancelBody(lastResponse)
      lastResponse = res
      lastCandidate = candidate
      continue
    }

    await cancelBody(lastResponse)
    return { kind: "response", candidate, response: res, lease }
  }

  await cancelBody(lastResponse)
  return {
    kind: "exhausted",
    tried: plan.ordered.slice(0, idx),
    lastBenched: ranOutOfCandidates && lastResponse ? lastCandidate : null,
  }
}
