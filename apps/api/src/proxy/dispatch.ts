import { decryptJson } from "../crypto/token_crypto"
import { recordEdgeTimeoutStrike } from "../db/accounts"
import type { CustomProviderRow } from "../db/custom_providers"
import type { Env, ProviderId } from "../env"
import { markBenched } from "../pool/bench"
import type { AcquiredAccount, StoredCredential } from "../pool/acquire"
import { getAdapter } from "../providers"
import { refreshAccountUsageInBackground } from "../providers/usage_refresh"
import type { ChatCompletionRequest, ProviderAdapter } from "../providers/types"
import { poolCandidates } from "../routing/candidates"
import { candidateFactsList, earliestUnusableUntil } from "../routing/facts"
import {
  EDGE_TIMEOUT_COOLDOWN_MS,
  isEdgeTimeoutStatus,
  penaltyForOutcome,
} from "../routing/feedback"
import { normalizeStrategy, orderCandidates } from "../routing/strategy"
import type { RoutingCandidate } from "../routing/types"
import { logRequest } from "../logging/request_log"
import {
  createAnthropicSseUsageSniffer,
  createOpenAISseUsageSniffer,
  fromAnthropicUsage,
  fromOpenAIUsage,
  type NormalizedUsage,
} from "../logging/usage_capture"
import { splitModelId } from "../utils/model"
import {
  streamWithEagerProducer,
  streamWithKeepalive,
  type StreamCloseReason,
} from "./sse"

/** Keeps a Worker invocation alive for a deferred `logRequest` past the returned Response — `c.executionCtx.waitUntil` in production, a test double in tests. */
export type WaitUntil = (promise: Promise<unknown>) => void

/** The candidate walk never retries more than this many real upstream calls in one request — same cap as the old `acquireAccount`+exclude loop. */
const MAX_ATTEMPTS = 8

/**
 * `request_logs.model`/`provider` always store the expanded canonical
 * target, never a model-group alias — reconstructed the same way
 * `splitModelId` builds `raw` (prefix + "/" + rest), so this is
 * byte-identical to `req.rawModel` whenever the client sent a direct
 * `provider/model` id and only diverges when a group expanded it
 * (docs/api.md "Model routing").
 */
function canonicalModelId(provider: string, upstreamModel: string): string {
  return `${provider}/${upstreamModel}`
}

/** No real upstream chunk for this long tears the stream down — docs/api.md "Streaming". */
const DEFAULT_IDLE_TIMEOUT_MS = 120_000

/** OpenAI-surface stall frame — exact text from docs/api.md "Streaming". */
const OPENAI_STALL_FRAME = new TextEncoder().encode(
  'data: {"error":{"message":"upstream stalled: no data received for 120s","type":"api_error","code":"upstream_stall"}}\n\n',
)

/** Anthropic-surface stall frame — exact text from docs/api.md "Streaming". */
const ANTHROPIC_STALL_FRAME = new TextEncoder().encode(
  'event: error\ndata: {"type":"error","error":{"type":"overloaded_error","message":"upstream stalled: no data received for 120s"}}\n\n',
)

/**
 * `request_logs.error_code` for a streamed response, from how it closed and
 * whether the sniffer ever saw the upstream's documented completion signal
 * — see docs/logging.md "Streaming rows". An idle-timeout close is always
 * `upstream_stall` regardless of completeness (the connection was abnormal
 * even if, by coincidence, a full payload had already arrived); anything
 * else that reached completion is unchanged (NULL); a client cancel before
 * completion is `client_abort`; any other close before completion — a clean
 * EOF with no completion signal, or a transport error — is `incomplete_stream`.
 */
function streamCloseErrorCode(reason: StreamCloseReason, complete: boolean): string | null {
  if (reason === "idle_timeout") return "upstream_stall"
  if (complete) return null
  if (reason === "cancel") return "client_abort"
  return "incomplete_stream"
}

/** Per-attempt upstream response-header deadline (docs/api.md "Keepalive and idle timeout"). */
const DEFAULT_FIRST_BYTE_TIMEOUT_MS = 180_000

/** Invalid, absent, or non-positive environment values use the documented default. */
function firstByteTimeoutMs(env: Env): number {
  const parsed = Number(env.UPSTREAM_FIRST_BYTE_TIMEOUT_MS)
  return Number.isFinite(parsed) && parsed > 0 ? Math.floor(parsed) : DEFAULT_FIRST_BYTE_TIMEOUT_MS
}

/** Wait only for upstream response headers, and always release the timer once fetch resolves or rejects. */
async function withFirstByteTimeout<T>(
  env: Env,
  attempt: (signal: AbortSignal) => Promise<T>,
): Promise<{ value: T } | { timedOut: true }> {
  const controller = new AbortController()
  let timedOut = false
  const timer = setTimeout(() => {
    timedOut = true
    controller.abort()
  }, firstByteTimeoutMs(env))
  try {
    return { value: await attempt(controller.signal) }
  } catch (error) {
    if (timedOut) return { timedOut: true }
    throw error
  } finally {
    clearTimeout(timer)
  }
}

function retryAfterSeconds(untilMs: number): number {
  return Math.min(60, Math.max(1, Math.ceil((untilMs - Date.now()) / 1000)))
}

function retryMarker(status: number, errorCode?: string | null): HeadersInit | undefined {
  if (status === 503 && errorCode === "upstream_unavailable") return { "x-should-retry": "true" }
  if (
    (status === 400 && (errorCode === "no_upstream_account" || errorCode === "invalid_model" || errorCode === "loop_detected")) ||
    (status === 429 && errorCode === "spend_limit_exceeded")
  ) return { "x-should-retry": "false" }
  return undefined
}

type TimedUpstreamResponse = { response: Response; timedOut: false } | { response: null; timedOut: true }

async function fetchUpstreamResponse(
  env: Env,
  attempt: (signal: AbortSignal) => Promise<Response>,
): Promise<TimedUpstreamResponse> {
  const result = await withFirstByteTimeout(env, attempt)
  return "timedOut" in result ? { response: null, timedOut: true } : { response: result.value, timedOut: false }
}

function waitForOverloadRetry(): Promise<void> {
  // Jitter keeps concurrent overloaded calls from immediately synchronizing;
  // zero is intentionally allowed in unit tests that stub the retry response.
  return new Promise((resolve) => setTimeout(resolve, 900 + Math.floor(Math.random() * 200)))
}

/** A 529 is transient fleet overload, so retry this account exactly once without benching it. */
async function retry529Once(
  call: () => Promise<TimedUpstreamResponse>,
): Promise<TimedUpstreamResponse> {
  let result = await call()
  if (result.timedOut || result.response.status !== 529) return result
  try {
    await result.response.body?.cancel()
  } catch {
    /* exhausted 529 body is irrelevant */
  }
  await waitForOverloadRetry()
  result = await call()
  return result
}

/**
 * Shared "pool unavailable" 503 for both surfaces — same envelope shape
 * either dispatch function already returned before `Retry-After` existed,
 * now with the header attached whenever the earliest bench/limit expiry
 * across the candidates is known (docs/api.md "Errors"): every candidate is
 * currently unusable, or the 8-attempt walk exhausted the ones that were.
 */
function upstreamUnavailableResponse(body: Record<string, unknown>, untilMs: number | null): Response {
  const headers = new Headers(retryMarker(503, "upstream_unavailable"))
  if (untilMs !== null) headers.set("retry-after", String(retryAfterSeconds(untilMs)))
  return Response.json(body, { status: 503, headers })
}

/** OpenAI SSE terminal error frame (eager commit / stall pattern). */
function openaiStreamErrorFrame(message: string, type: string, code?: string): Uint8Array {
  const error: Record<string, string> = { message, type }
  if (code) error.code = code
  return new TextEncoder().encode(`data: ${JSON.stringify({ error })}\n\n`)
}

/** Anthropic SSE terminal error event (eager commit / stall pattern). */
function anthropicStreamErrorFrame(message: string, type: string): Uint8Array {
  return new TextEncoder().encode(
    `event: error\ndata: ${JSON.stringify({ type: "error", error: { type, message } })}\n\n`,
  )
}

/** Best-effort message extraction from an upstream error JSON body. */
function messageFromUpstreamErrorBody(text: string, fallback: string): string {
  try {
    const j = JSON.parse(text) as {
      error?: { message?: string; type?: string; code?: string } | string
      message?: string
    }
    if (typeof j.error === "object" && j.error && typeof j.error.message === "string" && j.error.message) {
      return j.error.message
    }
    if (typeof j.error === "string" && j.error) return j.error
    if (typeof j.message === "string" && j.message) return j.message
  } catch {
    /* not JSON */
  }
  const trimmed = text.trim()
  return trimmed || fallback
}

function openaiErrorTypeFromBody(text: string): string {
  try {
    const j = JSON.parse(text) as { error?: { type?: string } }
    if (typeof j.error?.type === "string" && j.error.type) return j.error.type
  } catch {
    /* */
  }
  return "api_error"
}

function anthropicErrorTypeFromBody(text: string): string {
  try {
    const j = JSON.parse(text) as {
      error?: { type?: string }
      type?: string
    }
    if (typeof j.error?.type === "string" && j.error.type) return j.error.type
    if (j.type === "error" && typeof j.error === "object") return "api_error"
  } catch {
    /* */
  }
  return "api_error"
}

/** SSE response headers for eager commit (HTTP already 200 before upstream). */
function sseResponseHeaders(extra?: Headers): Headers {
  const out = new Headers()
  out.set("content-type", "text/event-stream; charset=utf-8")
  out.set("cache-control", "no-cache")
  if (extra) {
    const rl = [...extra.entries()].filter(([k]) => k.startsWith("anthropic-ratelimit-"))
    for (const [k, v] of rl) out.set(k, v)
  }
  return out
}

function bodyWantsStream(body: unknown): boolean {
  return (
    !!body &&
    typeof body === "object" &&
    (body as { stream?: unknown }).stream === true
  )
}

/**
 * The routing module's candidate list, ready for dispatch to walk
 * (docs/providers.md § Routing module). `candidates` and `strategy` let a
 * caller (a group dispatch, via the cross-target flattened list) hand in an
 * already-built, possibly cross-provider list; when omitted, this builds
 * the ordinary single-pool list dispatch has always used — the shape every
 * existing single-provider call site (including tests) still gets.
 */
type CandidateSource = {
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
}

type Plan =
  | { kind: "no_account" }
  | { kind: "unavailable"; untilMs: number | null }
  | { kind: "usable"; ordered: RoutingCandidate[] }

async function planCandidates(env: Env, src: CandidateSource, upstreamModel: string): Promise<Plan> {
  const candidates =
    src.candidates ??
    (await poolCandidates(env, src.userId, {
      provider: src.provider,
      upstreamModel,
      isBuiltin: src.isBuiltin ?? true,
      customProvider: src.customProvider,
      adapter: src.adapter ?? getAdapter(src.provider as ProviderId),
      accountId: src.pinnedAccountId ?? null,
    }))
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

/** Bench persistence is feedback, never a reason to abort an in-flight failover walk. */
async function persistBench(
  env: Env,
  userId: string,
  candidate: RoutingCandidate,
  cooldownMs: number,
  upstreamStatus: number,
): Promise<void> {
  try {
    await markBenched(env, userId, candidate.provider, candidate.account.id, cooldownMs, String(upstreamStatus))
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
async function persistEdgeTimeoutStrike(
  env: Env,
  userId: string,
  candidate: RoutingCandidate,
): Promise<boolean> {
  try {
    return await recordEdgeTimeoutStrike(env.DB, userId, candidate.provider, candidate.account.id)
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
  userId: string,
  candidate: RoutingCandidate,
  response: Response,
): Promise<boolean> {
  if (isEdgeTimeoutStatus(response.status)) {
    if (await persistEdgeTimeoutStrike(env, userId, candidate)) {
      await persistBench(env, userId, candidate, EDGE_TIMEOUT_COOLDOWN_MS, response.status)
    }
    return true
  }
  const penalty = penaltyForOutcome(response.status, response.headers, candidate.account)
  if (!penalty) return false
  await persistBench(env, userId, candidate, penalty.cooldownMs, response.status)
  return true
}

export async function dispatchChatCompletions(
  env: Env,
  opts: {
    userId: string
    apiKeyId: string | null
    /** Builtin `ProviderId` or a custom provider's slug. */
    provider: string
    /** Pre-resolved adapter for custom providers; defaults to the builtin registry. */
    adapter?: ProviderAdapter
    req: ChatCompletionRequest & { rawModel: string }
    waitUntil: WaitUntil
    /** Testability hook for the streaming idle timeout; defaults to 120_000. */
    idleTimeoutMs?: number
    /** The model-group alias this request was addressed to, if any — logged alongside the expanded canonical model (docs/database.md `request_logs.group_name`). */
    groupName?: string
    /** Model-group account pinning (docs/providers.md § Model groups): restrict acquire/failover to exactly this `upstream_accounts` row. Ignored when `candidates` is given (already scoped). */
    pinnedAccountId?: string
    /** Pre-built cross-target candidate list — group dispatch (docs/providers.md § Routing module). */
    candidates?: RoutingCandidate[]
    strategy?: string
    isBuiltin?: boolean
    customProvider?: CustomProviderRow
  },
): Promise<Response> {
  if (opts.req.stream) {
    return dispatchChatCompletionsEager(env, opts)
  }
  return dispatchChatCompletionsLegacy(env, opts)
}

type ChatCompletionsOpts = Parameters<typeof dispatchChatCompletions>[1]

/**
 * Eager streaming commit: return 200 + SSE immediately, run the candidate
 * walk inside the stream (docs/api.md "Eager streaming commit").
 */
async function dispatchChatCompletionsEager(env: Env, opts: ChatCompletionsOpts): Promise<Response> {
  const started = Date.now()
  const idleTimeoutMs = opts.idleTimeoutMs ?? DEFAULT_IDLE_TIMEOUT_MS

  let forcedErrorCode: string | null = null
  let accountId: string | undefined
  let usedProvider = opts.provider
  let usedUpstreamModel = opts.req.upstreamModel
  /** TTFB into the pipe, or time until terminal fail/cancel if earlier. */
  let headersLatencyMs: number | null = null
  let lastUpstreamStatus: number | null = null
  const sniffer = createOpenAISseUsageSniffer()

  const body = streamWithEagerProducer(
    async (ctl) => {
      const plan = await planCandidates(env, opts, opts.req.upstreamModel)
      if (ctl.cancelled()) return

      if (plan.kind === "no_account") {
        headersLatencyMs = Date.now() - started
        forcedErrorCode = "no_upstream_account"
        ctl.fail(
          openaiStreamErrorFrame(
            `No usable ${opts.provider} account for this user`,
            "invalid_request_error",
            "no_upstream_account",
          ),
        )
        return
      }
      if (plan.kind === "unavailable") {
        headersLatencyMs = Date.now() - started
        forcedErrorCode = "upstream_unavailable"
        ctl.fail(
          openaiStreamErrorFrame("All upstream accounts unavailable", "api_error", "upstream_unavailable"),
        )
        return
      }

      let lastResponse: Response | null = null
      let idx = 0
      let attempts = 0
      /** Set only when the candidate list itself ran dry (not the attempt cap) — see the post-loop branch below. */
      let ranOutOfCandidates = false
      for (; attempts < MAX_ATTEMPTS; ) {
        if (ctl.cancelled()) return
        const candidate = plan.ordered[idx]
        if (!candidate) {
          ranOutOfCandidates = true
          break
        }
        idx++

        const acquired = await acquireCandidate(env, candidate)
        if (ctl.cancelled()) return
        if (!acquired) continue
        attempts++
        usedProvider = candidate.provider
        usedUpstreamModel = candidate.upstreamModel
        accountId = candidate.account.id

        let fetched: TimedUpstreamResponse
        try {
          fetched = await retry529Once(() =>
            fetchUpstreamResponse(env, (signal) =>
              candidate.adapter.chatCompletions(
                env,
                acquired,
                { ...opts.req, upstreamModel: candidate.upstreamModel },
                { apiKeyId: opts.apiKeyId, waitUntil: opts.waitUntil, signal },
              ),
            ),
          )
        } catch {
          headersLatencyMs = Date.now() - started
          forcedErrorCode = "upstream_error"
          ctl.fail(openaiStreamErrorFrame("upstream error", "api_error", "upstream_error"))
          return
        }
        if (fetched.timedOut) continue
        const res = fetched.response
        lastUpstreamStatus = res.status
        opts.waitUntil(refreshAccountUsageInBackground(env, candidate.account, candidate.adapter))
        if (ctl.cancelled()) {
          try {
            await res.body?.cancel()
          } catch {
            /* */
          }
          return
        }

        if (await shouldFailOverForResponse(env, opts.userId, candidate, res)) {
          if (lastResponse) {
            try {
              await lastResponse.body?.cancel()
            } catch {
              /* */
            }
          }
          lastResponse = res
          continue
        }

        // Non-bench response — success stream or terminal upstream error.
        if (res.body && res.ok && isEventStream(res)) {
          headersLatencyMs = Date.now() - started
          await ctl.pipeUpstream(res.body, {
            tap: (chunk) => sniffer.feed(chunk),
            idleTimeoutMs,
            stallFrame: OPENAI_STALL_FRAME,
          })
          return
        }

        const text = await safeResponseText(res)
        headersLatencyMs = Date.now() - started
        if (!res.ok) {
          forcedErrorCode = "upstream_error"
          ctl.fail(
            openaiStreamErrorFrame(
              messageFromUpstreamErrorBody(text, "upstream error"),
              openaiErrorTypeFromBody(text),
              "upstream_error",
            ),
          )
          return
        }
        // 200 but not event-stream under stream:true — nothing useful to pipe.
        ctl.close()
        return
      }

      headersLatencyMs = Date.now() - started
      if (ranOutOfCandidates && lastResponse) {
        try {
          await lastResponse.body?.cancel()
        } catch {
          /* */
        }
        forcedErrorCode = "upstream_unavailable"
        ctl.fail(
          openaiStreamErrorFrame("All upstream accounts unavailable", "api_error", "upstream_unavailable"),
        )
        return
      }
      // Attempt cap reached with only bench continues, or every remaining
      // candidate was undecryptable — synthesize the standard unavailable error.
      forcedErrorCode = "upstream_unavailable"
      ctl.fail(
        openaiStreamErrorFrame("All upstream accounts unavailable", "api_error", "upstream_unavailable"),
      )
    },
    undefined,
    {
      errorFrame: openaiStreamErrorFrame("upstream error", "api_error", "upstream_error"),
      onClose: (reason) => {
        const usage = sniffer.finish()
        const errorCode =
          forcedErrorCode ?? streamCloseErrorCode(reason, sniffer.complete())
        const latencyMs = headersLatencyMs ?? Date.now() - started
        opts.waitUntil(
          logRequest(env, {
            userId: opts.userId,
            apiKeyId: opts.apiKeyId,
            provider: usedProvider,
            model: canonicalModelId(usedProvider, usedUpstreamModel),
            accountId,
            statusCode: 200,
            latencyMs,
            errorCode,
            upstreamStatus: lastUpstreamStatus,
            groupName: opts.groupName ?? null,
            ...usageFields(usage),
          }),
        )
      },
    },
  )

  return new Response(body, {
    status: 200,
    headers: sseResponseHeaders(),
  })
}

/**
 * Non-stream path (and legacy attach when upstream unexpectedly returns
 * event-stream without client stream:true). HTTP status mirrors upstream /
 * pool errors exactly — existing tests depend on 400/503 pass-through.
 */
async function dispatchChatCompletionsLegacy(env: Env, opts: ChatCompletionsOpts): Promise<Response> {
  const started = Date.now()
  const idleTimeoutMs = opts.idleTimeoutMs ?? DEFAULT_IDLE_TIMEOUT_MS

  const plan = await planCandidates(env, opts, opts.req.upstreamModel)
  if (plan.kind === "no_account") {
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider: opts.provider,
      model: canonicalModelId(opts.provider, opts.req.upstreamModel),
      statusCode: 400,
      latencyMs: Date.now() - started,
      errorCode: "no_upstream_account",
      groupName: opts.groupName ?? null,
    })
    return Response.json(
      {
        error: {
          message: `No usable ${opts.provider} account for this user`,
          type: "invalid_request_error",
          code: "no_upstream_account",
        },
      },
      { status: 400, headers: retryMarker(400, "no_upstream_account") },
    )
  }
  if (plan.kind === "unavailable") {
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider: opts.provider,
      model: canonicalModelId(opts.provider, opts.req.upstreamModel),
      statusCode: 503,
      latencyMs: Date.now() - started,
      errorCode: "upstream_unavailable",
      groupName: opts.groupName ?? null,
    })
    return upstreamUnavailableResponse(
      { error: { message: "All upstream accounts unavailable", code: "upstream_unavailable" } },
      plan.untilMs,
    )
  }

  let lastResponse: Response | null = null
  let lastCandidate: RoutingCandidate | null = null
  let lastUpstreamStatus: number | null = null
  let idx = 0
  let attempts = 0
  let ranOutOfCandidates = false
  for (; attempts < MAX_ATTEMPTS; ) {
    const candidate = plan.ordered[idx]
    if (!candidate) {
      ranOutOfCandidates = true
      break
    }
    idx++

    const acquired = await acquireCandidate(env, candidate)
    if (!acquired) continue
    attempts++

    let fetched: TimedUpstreamResponse
    try {
      fetched = await retry529Once(() =>
        fetchUpstreamResponse(env, (signal) =>
          candidate.adapter.chatCompletions(
            env,
            acquired,
            { ...opts.req, upstreamModel: candidate.upstreamModel },
            { apiKeyId: opts.apiKeyId, waitUntil: opts.waitUntil, signal },
          ),
        ),
      )
    } catch {
      const latencyMs = Date.now() - started
      await logRequest(env, {
        userId: opts.userId,
        apiKeyId: opts.apiKeyId,
        provider: candidate.provider,
        model: canonicalModelId(candidate.provider, candidate.upstreamModel),
        accountId: candidate.account.id,
        statusCode: 502,
        latencyMs,
        errorCode: "upstream_error",
        groupName: opts.groupName ?? null,
      })
      return Response.json({ error: { message: "upstream error", code: "upstream_error" } }, { status: 502 })
    }
    if (fetched.timedOut) continue
    const res = fetched.response
    lastUpstreamStatus = res.status
    opts.waitUntil(refreshAccountUsageInBackground(env, candidate.account, candidate.adapter))

    if (await shouldFailOverForResponse(env, opts.userId, candidate, res)) {
      if (lastResponse) {
        try {
          await lastResponse.body?.cancel()
        } catch {
          /* */
        }
      }
      lastResponse = res
      lastCandidate = candidate
      continue
    }

    const latencyMs = Date.now() - started
    if (res.body && isEventStream(res)) {
      // Legacy attach: client did not ask stream but upstream returned SSE.
      const sniffer = createOpenAISseUsageSniffer()
      const streamBody = streamWithKeepalive(res.body, undefined, {
        tap: (chunk) => sniffer.feed(chunk),
        idleTimeoutMs,
        stallFrame: OPENAI_STALL_FRAME,
        onClose: (reason) => {
          const usage = sniffer.finish()
          const errorCode = streamCloseErrorCode(reason, sniffer.complete())
          opts.waitUntil(
            logRequest(env, {
              userId: opts.userId,
              apiKeyId: opts.apiKeyId,
              provider: candidate.provider,
              model: canonicalModelId(candidate.provider, candidate.upstreamModel),
              accountId: candidate.account.id,
              statusCode: res.status,
              latencyMs,
              errorCode,
              upstreamStatus: res.status,
              groupName: opts.groupName ?? null,
              ...usageFields(usage),
            }),
          )
        },
      })
      return new Response(streamBody, {
        status: res.status,
        headers: passthroughStreamHeaders(res.headers),
      })
    }

    // clone for logging tokens if json
    const text = await res.text()
    let usage: NormalizedUsage | null = null
    try {
      const j = JSON.parse(text) as { usage?: Record<string, unknown> }
      usage = fromOpenAIUsage(j.usage)
    } catch {
      /* */
    }
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider: candidate.provider,
      model: canonicalModelId(candidate.provider, candidate.upstreamModel),
      accountId: candidate.account.id,
      statusCode: res.status,
      latencyMs,
      upstreamStatus: res.status,
      groupName: opts.groupName ?? null,
      ...usageFields(usage),
    })
    return new Response(text, {
      status: res.status,
      headers: { "content-type": res.headers.get("content-type") || "application/json" },
    })
  }

  if (ranOutOfCandidates && lastResponse && lastCandidate) {
    try {
      await lastResponse.body?.cancel()
    } catch {
      /* */
    }
    const untilMs = await recomputeUnavailableUntil(env, opts.userId, plan.ordered.slice(0, idx))
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider: lastCandidate.provider,
      model: canonicalModelId(lastCandidate.provider, lastCandidate.upstreamModel),
      accountId: lastCandidate.account.id,
      statusCode: 503,
      latencyMs: Date.now() - started,
      errorCode: "upstream_unavailable",
      upstreamStatus: lastUpstreamStatus,
      groupName: opts.groupName ?? null,
    })
    return upstreamUnavailableResponse(
      { error: { message: "All upstream accounts unavailable", code: "upstream_unavailable" } },
      untilMs,
    )
  }

  // Attempt cap reached with only bench continues (or the remainder of the
  // list was undecryptable) — same 503 + Retry-After as the all-unusable
  // case, recomputed now that this loop just benched more of the pool.
  const untilMs = await recomputeUnavailableUntil(env, opts.userId, plan.ordered.slice(0, idx))
  await logRequest(env, {
    userId: opts.userId,
    apiKeyId: opts.apiKeyId,
    provider: opts.provider,
    model: canonicalModelId(opts.provider, opts.req.upstreamModel),
    statusCode: 503,
    latencyMs: Date.now() - started,
    errorCode: "upstream_unavailable",
    upstreamStatus: lastUpstreamStatus,
    groupName: opts.groupName ?? null,
  })
  return upstreamUnavailableResponse(
    { error: { message: "All upstream accounts unavailable", code: "upstream_unavailable" } },
    untilMs,
  )
}

/**
 * Recomputes the earliest bench/limit expiry across exactly the candidates
 * this walk just tried and benched — same "exclude set" the old loop used
 * for its bottom-of-loop `Retry-After`, expressed as a fresh facts read
 * (cheap: bounded by `MAX_ATTEMPTS`, and correct even though a 429's
 * cooldown can vary per candidate, unlike the old flat 300s-for-everything
 * assumption baked into `earliestBenchExpiry`).
 */
async function recomputeUnavailableUntil(
  env: Env,
  userId: string,
  tried: RoutingCandidate[],
): Promise<number | null> {
  if (tried.length === 0) return null
  const facts = await candidateFactsList(env, userId, tried)
  return earliestUnusableUntil(facts)
}

/**
 * Native Anthropic Messages (or count_tokens) passthrough — claude-code by
 * default, or a custom anthropic-format provider when `provider`/`adapter`
 * are given. cache_control is never rewritten — body goes through the
 * adapter method as-is aside from whatever fixed prepend that adapter itself
 * applies (claude-code only) and the `model` rewrite to the acquired
 * candidate's own upstream id. `endpoint` selects which adapter method
 * carries the request; both share this same candidate walk rather than
 * duplicating it.
 */
export async function dispatchAnthropicMessages(
  env: Env,
  opts: {
    userId: string
    apiKeyId: string | null
    body: unknown
    headers: Headers
    /** Canonical `provider/upstreamModel` for `request_logs.model` — never a group alias. */
    model: string
    /** Builtin `ProviderId` or a custom provider's slug. Defaults to claude-code. */
    provider?: string
    /** Pre-resolved adapter for custom providers; defaults to the builtin registry. */
    adapter?: ProviderAdapter
    endpoint?: "messages" | "count_tokens"
    waitUntil: WaitUntil
    /** Testability hook for the streaming idle timeout; defaults to 120_000. */
    idleTimeoutMs?: number
    /** The model-group alias this request was addressed to, if any (docs/database.md `request_logs.group_name`). */
    groupName?: string
    /** Model-group account pinning (docs/providers.md § Model groups): restrict acquire/failover to exactly this `upstream_accounts` row. Applies to `count_tokens` too — same loop. Ignored when `candidates` is given. */
    pinnedAccountId?: string
    /** Pre-built cross-target candidate list — group dispatch (docs/providers.md § Routing module). Every candidate's adapter must support `endpoint`. */
    candidates?: RoutingCandidate[]
    strategy?: string
    isBuiltin?: boolean
    customProvider?: CustomProviderRow
  },
): Promise<Response> {
  const endpoint = opts.endpoint ?? "messages"
  if (endpoint === "messages" && bodyWantsStream(opts.body)) {
    return dispatchAnthropicMessagesEager(env, opts)
  }
  return dispatchAnthropicMessagesLegacy(env, opts)
}

type AnthropicMessagesOpts = Parameters<typeof dispatchAnthropicMessages>[1]

/** `body` with `.model` rewritten to the candidate's own upstream id — cache_control and everything else pass through untouched. */
function bodyForCandidate(body: unknown, upstreamModel: string): unknown {
  return { ...(body as Record<string, unknown>), model: upstreamModel }
}

/** Bare upstream id from a canonical `provider/upstreamModel` string — used to build the single-pool candidate when dispatch wasn't handed a pre-built list. */
function upstreamModelFromCanonical(model: string): string {
  return splitModelId(model)?.upstreamModel ?? model
}

async function dispatchAnthropicMessagesEager(env: Env, opts: AnthropicMessagesOpts): Promise<Response> {
  const started = Date.now()
  const provider = opts.provider ?? "claude-code"
  const idleTimeoutMs = opts.idleTimeoutMs ?? DEFAULT_IDLE_TIMEOUT_MS
  const upstreamModel = upstreamModelFromCanonical(opts.model)

  // Fixed adapter (no candidates): keep the old immediate rejection for an
  // adapter that never supports `messages` at all, before ever touching the
  // pool — 500, not a synthesized pool-exhaustion error.
  if (!opts.candidates) {
    const fixedAdapter = opts.adapter ?? getAdapter(provider as ProviderId)
    if (!fixedAdapter.messages) {
      return Response.json(
        { type: "error", error: { type: "api_error", message: "messages not supported" } },
        { status: 500 },
      )
    }
  }

  let forcedErrorCode: string | null = null
  let accountId: string | undefined
  let usedProvider = provider
  let headersLatencyMs: number | null = null
  let lastUpstreamStatus: number | null = null
  const sniffer = createAnthropicSseUsageSniffer()

  const body = streamWithEagerProducer(
    async (ctl) => {
      const plan = await planCandidates(env, { ...opts, provider }, upstreamModel)
      if (ctl.cancelled()) return

      if (plan.kind === "no_account") {
        headersLatencyMs = Date.now() - started
        forcedErrorCode = "no_upstream_account"
        ctl.fail(anthropicStreamErrorFrame(`No usable ${provider} account`, "invalid_request_error"))
        return
      }
      if (plan.kind === "unavailable") {
        headersLatencyMs = Date.now() - started
        forcedErrorCode = "upstream_unavailable"
        ctl.fail(anthropicStreamErrorFrame("upstream_unavailable", "api_error"))
        return
      }

      let lastResponse: Response | null = null
      let idx = 0
      let attempts = 0
      let ranOutOfCandidates = false
      for (; attempts < MAX_ATTEMPTS; ) {
        if (ctl.cancelled()) return
        const candidate = plan.ordered[idx]
        if (!candidate) {
          ranOutOfCandidates = true
          break
        }
        idx++
        const call = candidate.adapter.messages
        if (!call) continue

        const acquired = await acquireCandidate(env, candidate)
        if (ctl.cancelled()) return
        if (!acquired) continue
        attempts++
        usedProvider = candidate.provider
        accountId = candidate.account.id

        let fetched: TimedUpstreamResponse
        try {
          fetched = await retry529Once(() =>
            fetchUpstreamResponse(env, (signal) =>
              call(env, acquired, bodyForCandidate(opts.body, candidate.upstreamModel), opts.headers, {
                waitUntil: opts.waitUntil,
                signal,
              }),
            ),
          )
        } catch {
          headersLatencyMs = Date.now() - started
          forcedErrorCode = "upstream_error"
          ctl.fail(anthropicStreamErrorFrame("upstream error", "api_error"))
          return
        }
        if (fetched.timedOut) continue
        const res = fetched.response
        lastUpstreamStatus = res.status
        opts.waitUntil(refreshAccountUsageInBackground(env, candidate.account, candidate.adapter))
        if (ctl.cancelled()) {
          try {
            await res.body?.cancel()
          } catch {
            /* */
          }
          return
        }

        if (await shouldFailOverForResponse(env, opts.userId, candidate, res)) {
          if (lastResponse) {
            try {
              await lastResponse.body?.cancel()
            } catch {
              /* */
            }
          }
          lastResponse = res
          continue
        }

        if (res.body && res.ok && isEventStream(res)) {
          headersLatencyMs = Date.now() - started
          await ctl.pipeUpstream(res.body, {
            tap: (chunk) => sniffer.feed(chunk),
            idleTimeoutMs,
            stallFrame: ANTHROPIC_STALL_FRAME,
          })
          return
        }

        const text = await safeResponseText(res)
        headersLatencyMs = Date.now() - started
        if (!res.ok) {
          forcedErrorCode = "upstream_error"
          ctl.fail(
            anthropicStreamErrorFrame(
              messageFromUpstreamErrorBody(text, "upstream error"),
              anthropicErrorTypeFromBody(text),
            ),
          )
          return
        }
        ctl.close()
        return
      }

      headersLatencyMs = Date.now() - started
      if (ranOutOfCandidates && lastResponse) {
        try {
          await lastResponse.body?.cancel()
        } catch {
          /* */
        }
        forcedErrorCode = "upstream_unavailable"
        ctl.fail(anthropicStreamErrorFrame("upstream_unavailable", "api_error"))
        return
      }
      forcedErrorCode = "upstream_unavailable"
      ctl.fail(anthropicStreamErrorFrame("upstream_unavailable", "api_error"))
    },
    undefined,
    {
      errorFrame: anthropicStreamErrorFrame("upstream error", "api_error"),
      onClose: (reason) => {
        const usage = sniffer.finish()
        const errorCode =
          forcedErrorCode ?? streamCloseErrorCode(reason, sniffer.complete())
        const latencyMs = headersLatencyMs ?? Date.now() - started
        opts.waitUntil(
          logRequest(env, {
            userId: opts.userId,
            apiKeyId: opts.apiKeyId,
            provider: usedProvider,
            model: opts.model,
            accountId,
            statusCode: 200,
            latencyMs,
            errorCode,
            upstreamStatus: lastUpstreamStatus,
            groupName: opts.groupName ?? null,
            ...usageFields(usage),
          }),
        )
      },
    },
  )

  return new Response(body, {
    status: 200,
    headers: sseResponseHeaders(),
  })
}

async function dispatchAnthropicMessagesLegacy(env: Env, opts: AnthropicMessagesOpts): Promise<Response> {
  const started = Date.now()
  const provider = opts.provider ?? "claude-code"
  const endpoint = opts.endpoint ?? "messages"
  const idleTimeoutMs = opts.idleTimeoutMs ?? DEFAULT_IDLE_TIMEOUT_MS
  const upstreamModel = upstreamModelFromCanonical(opts.model)

  // Fixed adapter (no candidates / not builtin-driven per-candidate): keep
  // the old single-call rejection for an adapter that never supports this
  // endpoint at all — e.g. a fixed non-builtin adapter test double.
  if (!opts.candidates) {
    const fixedAdapter = opts.adapter ?? getAdapter(provider as ProviderId)
    const fixedCall = endpoint === "count_tokens" ? fixedAdapter.countTokens : fixedAdapter.messages
    if (!fixedCall) {
      return Response.json(
        { type: "error", error: { type: "api_error", message: `${endpoint} not supported` } },
        { status: 500 },
      )
    }
  }

  const plan = await planCandidates(env, { ...opts, provider }, upstreamModel)
  if (plan.kind === "no_account") {
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider,
      model: opts.model,
      statusCode: 400,
      latencyMs: Date.now() - started,
      errorCode: "no_upstream_account",
      groupName: opts.groupName ?? null,
    })
    return Response.json(
      {
        type: "error",
        error: { type: "invalid_request_error", message: `No usable ${provider} account` },
      },
      { status: 400, headers: retryMarker(400, "no_upstream_account") },
    )
  }
  if (plan.kind === "unavailable") {
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider,
      model: opts.model,
      statusCode: 503,
      latencyMs: Date.now() - started,
      errorCode: "upstream_unavailable",
      groupName: opts.groupName ?? null,
    })
    return upstreamUnavailableResponse(
      { type: "error", error: { type: "api_error", message: "upstream_unavailable" } },
      plan.untilMs,
    )
  }

  let lastResponse: Response | null = null
  let lastCandidate: RoutingCandidate | null = null
  let lastUpstreamStatus: number | null = null
  let idx = 0
  let attempts = 0
  let ranOutOfCandidates = false
  for (; attempts < MAX_ATTEMPTS; ) {
    const candidate = plan.ordered[idx]
    if (!candidate) {
      ranOutOfCandidates = true
      break
    }
    idx++
    const call = endpoint === "count_tokens" ? candidate.adapter.countTokens : candidate.adapter.messages
    if (!call) continue

    const acquired = await acquireCandidate(env, candidate)
    if (!acquired) continue
    attempts++

    let fetched: TimedUpstreamResponse
    try {
      fetched = await retry529Once(() =>
        fetchUpstreamResponse(env, (signal) =>
          call(env, acquired, bodyForCandidate(opts.body, candidate.upstreamModel), opts.headers, {
            waitUntil: opts.waitUntil,
            signal,
          }),
        ),
      )
    } catch {
      const latencyMs = Date.now() - started
      await logRequest(env, {
        userId: opts.userId,
        apiKeyId: opts.apiKeyId,
        provider: candidate.provider,
        model: canonicalModelId(candidate.provider, candidate.upstreamModel),
        accountId: candidate.account.id,
        statusCode: 502,
        latencyMs,
        errorCode: "upstream_error",
        groupName: opts.groupName ?? null,
      })
      return Response.json(
        { type: "error", error: { type: "api_error", message: "upstream error" } },
        { status: 502 },
      )
    }
    if (fetched.timedOut) continue
    const res = fetched.response
    lastUpstreamStatus = res.status
    opts.waitUntil(refreshAccountUsageInBackground(env, candidate.account, candidate.adapter))

    if (await shouldFailOverForResponse(env, opts.userId, candidate, res)) {
      if (lastResponse) {
        try {
          await lastResponse.body?.cancel()
        } catch {
          /* */
        }
      }
      lastResponse = res
      lastCandidate = candidate
      continue
    }

    const latencyMs = Date.now() - started
    // count_tokens estimates tokens, it never consumes them — never log a
    // count from that endpoint, streaming or not (it never streams anyway).
    const isCountTokens = endpoint === "count_tokens"
    if (res.body && isEventStream(res)) {
      const sniffer = createAnthropicSseUsageSniffer()
      const streamBody = streamWithKeepalive(res.body, undefined, {
        tap: (chunk) => sniffer.feed(chunk),
        idleTimeoutMs,
        stallFrame: ANTHROPIC_STALL_FRAME,
        onClose: (reason) => {
          const usage = isCountTokens ? null : sniffer.finish()
          const errorCode = isCountTokens ? null : streamCloseErrorCode(reason, sniffer.complete())
          opts.waitUntil(
            logRequest(env, {
              userId: opts.userId,
              apiKeyId: opts.apiKeyId,
              provider: candidate.provider,
              model: canonicalModelId(candidate.provider, candidate.upstreamModel),
              accountId: candidate.account.id,
              statusCode: res.status,
              latencyMs,
              errorCode,
              upstreamStatus: res.status,
              groupName: opts.groupName ?? null,
              ...usageFields(usage),
            }),
          )
        },
      })
      return new Response(streamBody, {
        status: res.status,
        headers: passthroughStreamHeaders(res.headers),
      })
    }

    // Peek at the body for usage without disturbing what we return to the
    // client — clone rather than reconstruct, so every upstream header
    // (rate-limit headers included) still passes through untouched.
    let usage: NormalizedUsage | null = null
    if (!isCountTokens) {
      try {
        const json = (await res.clone().json()) as { usage?: Record<string, unknown> }
        usage = fromAnthropicUsage(json.usage)
      } catch {
        /* not JSON, or no body — keep NULL usage */
      }
    }
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider: candidate.provider,
      model: canonicalModelId(candidate.provider, candidate.upstreamModel),
      accountId: candidate.account.id,
      statusCode: res.status,
      latencyMs,
      upstreamStatus: res.status,
      groupName: opts.groupName ?? null,
      ...usageFields(usage),
    })
    return res
  }

  if (ranOutOfCandidates && lastResponse && lastCandidate) {
    try {
      await lastResponse.body?.cancel()
    } catch {
      /* */
    }
    const untilMs = await recomputeUnavailableUntil(env, opts.userId, plan.ordered.slice(0, idx))
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider: lastCandidate.provider,
      model: canonicalModelId(lastCandidate.provider, lastCandidate.upstreamModel),
      accountId: lastCandidate.account.id,
      statusCode: 503,
      latencyMs: Date.now() - started,
      errorCode: "upstream_unavailable",
      upstreamStatus: lastUpstreamStatus,
      groupName: opts.groupName ?? null,
    })
    return upstreamUnavailableResponse(
      { type: "error", error: { type: "api_error", message: "upstream_unavailable" } },
      untilMs,
    )
  }

  const untilMs = await recomputeUnavailableUntil(env, opts.userId, plan.ordered.slice(0, idx))
  await logRequest(env, {
    userId: opts.userId,
    apiKeyId: opts.apiKeyId,
    provider,
    model: opts.model,
    statusCode: 503,
    latencyMs: Date.now() - started,
    errorCode: "upstream_unavailable",
    upstreamStatus: lastUpstreamStatus,
    groupName: opts.groupName ?? null,
  })
  return upstreamUnavailableResponse(
    { type: "error", error: { type: "api_error", message: "upstream_unavailable" } },
    untilMs,
  )
}

/**
 * Anthropic Messages ingress for non-Claude providers:
 * convert → OpenAI chat path → convert response back.
 * Strips cache_control on convert; never invents Grok affinity ids
 * (forward client headers via req.affinity only).
 */
export async function dispatchAnthropicViaOpenAI(
  env: Env,
  opts: {
    userId: string
    apiKeyId: string | null
    /** Builtin `ProviderId` or a custom provider's slug. */
    provider: string
    /** Pre-resolved adapter for custom providers; defaults to the builtin registry. */
    adapter?: ProviderAdapter
    rawModel: string
    upstreamModel: string
    body: Record<string, unknown>
    affinity?: ChatCompletionRequest["affinity"]
    waitUntil: WaitUntil
    /** The model-group alias this request was addressed to, if any (docs/database.md `request_logs.group_name`). */
    groupName?: string
    /** Model-group account pinning (docs/providers.md § Model groups): restrict acquire/failover to exactly this `upstream_accounts` row. Ignored when `candidates` is given. */
    pinnedAccountId?: string
    /** Pre-built cross-target candidate list — group dispatch (docs/providers.md § Routing module). */
    candidates?: RoutingCandidate[]
    strategy?: string
    isBuiltin?: boolean
    customProvider?: CustomProviderRow
  },
): Promise<Response> {
  const {
    anthropicToOpenAIChatRequest,
    openaiToAnthropicMessage,
    openaiSseToAnthropicStream,
    promptCacheKeyFromAnthropicMetadata,
  } = await import("./openai_anthropic")
  const { parseReasoningEffort } = await import("../utils/reasoning")

  const converted = anthropicToOpenAIChatRequest(opts.body)
  const effort = parseReasoningEffort(converted.reasoning_effort)
  if (effort === "invalid") {
    return Response.json(
      {
        type: "error",
        error: {
          type: "invalid_request_error",
          message: "invalid reasoning_effort",
        },
      },
      { status: 400 },
    )
  }

  const openaiRes = await dispatchChatCompletions(env, {
    userId: opts.userId,
    apiKeyId: opts.apiKeyId,
    provider: opts.provider,
    adapter: opts.adapter,
    waitUntil: opts.waitUntil,
    groupName: opts.groupName,
    pinnedAccountId: opts.pinnedAccountId,
    candidates: opts.candidates,
    strategy: opts.strategy,
    isBuiltin: opts.isBuiltin,
    customProvider: opts.customProvider,
    req: {
      model: opts.rawModel,
      rawModel: opts.rawModel,
      upstreamModel: opts.upstreamModel,
      messages: converted.messages,
      stream: converted.stream,
      max_tokens: converted.max_tokens,
      tools: converted.tools,
      tool_choice: converted.tool_choice,
      response_format: converted.response_format,
      reasoning_effort: effort,
      temperature: converted.temperature,
      top_p: converted.top_p,
      stop: converted.stop,
      // Named field only — never added to the converted rawBody, so the
      // custom-openai passthrough body is byte-identical to before.
      prompt_cache_key: promptCacheKeyFromAnthropicMetadata(opts.body),
      affinity: opts.affinity,
      // OpenAI-shaped body for the custom-openai passthrough adapter; a
      // no-op for built-ins, which build their own body from named fields.
      rawBody: converted as unknown as Record<string, unknown>,
    },
  })

  // Stream: OpenAI SSE → Anthropic SSE
  if (openaiRes.body && (converted.stream || isEventStream(openaiRes))) {
    if (!openaiRes.ok) {
      // pass error body through, map envelope if JSON
      const text = await openaiRes.text()
      return anthropicErrorFromOpenAIText(text, openaiRes.status)
    }
    // Keepalive again on the converted stream: the upstream wrapper's comments
    // are consumed by the converter, and no Anthropic event is emitted until
    // the first token — a long reasoning turn would otherwise send zero bytes.
    // With eager commit, dispatchChatCompletions already returned 200 + SSE;
    // openaiSseToAnthropicStream converts OpenAI error lines to Anthropic error events.
    return new Response(
      streamWithKeepalive(openaiSseToAnthropicStream(openaiRes.body, opts.rawModel)),
      {
        status: openaiRes.status,
        headers: passthroughStreamHeaders(openaiRes.headers),
      },
    )
  }

  const text = await openaiRes.text()
  if (!openaiRes.ok) {
    return anthropicErrorFromOpenAIText(text, openaiRes.status)
  }
  try {
    const json = JSON.parse(text) as Record<string, unknown>
    const msg = openaiToAnthropicMessage(json, opts.rawModel)
    return Response.json(msg, {
      status: 200,
      headers: { "content-type": "application/json" },
    })
  } catch {
    return new Response(text, {
      status: openaiRes.status,
      headers: { "content-type": openaiRes.headers.get("content-type") || "application/json" },
    })
  }
}

function anthropicErrorFromOpenAIText(text: string, status: number): Response {
  try {
    const j = JSON.parse(text) as {
      error?: { message?: string; type?: string; code?: string }
      type?: string
    }
    const message =
      j.error?.message ||
      (typeof j === "object" ? text : "upstream error")
    return Response.json(
      {
        type: "error",
        error: {
          type: j.error?.type || "api_error",
          message,
        },
      },
      { status },
    )
  } catch {
    return Response.json(
      {
        type: "error",
        error: { type: "api_error", message: text || "upstream error" },
      },
      { status },
    )
  }
}

function isEventStream(res: Response): boolean {
  const ct = res.headers.get("content-type") || ""
  return ct.includes("text/event-stream")
}

function passthroughStreamHeaders(h: Headers): Headers {
  const out = new Headers()
  out.set("content-type", h.get("content-type") || "text/event-stream; charset=utf-8")
  out.set("cache-control", "no-cache")
  const rl = [...h.entries()].filter(([k]) => k.startsWith("anthropic-ratelimit-"))
  for (const [k, v] of rl) out.set(k, v)
  return out
}

async function safeResponseText(res: Response): Promise<string> {
  try {
    return await res.text()
  } catch {
    return ""
  }
}

/** `null` (nothing captured) flattens to all-NULL request_logs token fields. */
function usageFields(usage: NormalizedUsage | null): {
  promptTokens: number | null
  completionTokens: number | null
  cacheReadInputTokens: number | null
  cacheCreationInputTokens: number | null
} {
  return {
    promptTokens: usage?.promptTokens ?? null,
    completionTokens: usage?.completionTokens ?? null,
    cacheReadInputTokens: usage?.cacheReadInputTokens ?? null,
    cacheCreationInputTokens: usage?.cacheCreationInputTokens ?? null,
  }
}

function streamWithUsageTap(
  upstream: ReadableStream<Uint8Array>,
  isJson: boolean,
  onFinish: (usage: NormalizedUsage | null, reason: StreamCloseReason) => void,
): ReadableStream<Uint8Array> {
  let finished = false
  let boundedBuf = ""
  let boundedLen = 0
  const MAX_TAP = 256 * 1024
  const decoder = new TextDecoder()
  const reader = upstream.getReader()

  const finishOnce = (reason: StreamCloseReason) => {
    if (finished) return
    finished = true
    let usage: NormalizedUsage | null = null
    if (isJson && boundedBuf) {
      try {
        const j = JSON.parse(boundedBuf) as { usage?: Record<string, unknown> }
        usage = fromOpenAIUsage(j.usage)
      } catch {
        /* malformed or truncated JSON */
      }
    }
    onFinish(usage, reason)
  }

  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        const { done, value } = await reader.read()
        if (done) {
          controller.close()
          finishOnce("done")
          return
        }
        if (value) {
          controller.enqueue(value)
          if (isJson && boundedLen < MAX_TAP) {
            try {
              boundedBuf += decoder.decode(value, { stream: true })
              boundedLen += value.byteLength
            } catch {
              /* */
            }
          }
        }
      } catch (err) {
        controller.error(err)
        finishOnce("error")
      }
    },
    async cancel(reason) {
      try {
        await reader.cancel(reason)
      } catch {
        /* */
      }
      finishOnce("cancel")
    },
  })
}

export async function dispatchAudioTranscriptions(
  env: Env,
  opts: {
    userId: string
    apiKeyId: string | null
    provider: string
    adapter?: ProviderAdapter
    formData: FormData
    rawModel: string
    upstreamModel: string
    waitUntil: WaitUntil
    idleTimeoutMs?: number
    groupName?: string
    pinnedAccountId?: string
    candidates?: RoutingCandidate[]
    strategy?: string
    isBuiltin?: boolean
    customProvider?: CustomProviderRow
  },
): Promise<Response> {
  const started = Date.now()
  const idleTimeoutMs = opts.idleTimeoutMs ?? DEFAULT_IDLE_TIMEOUT_MS

  const plan = await planCandidates(env, opts, opts.upstreamModel)
  if (plan.kind === "no_account") {
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider: opts.provider,
      model: canonicalModelId(opts.provider, opts.upstreamModel),
      statusCode: 400,
      latencyMs: Date.now() - started,
      errorCode: "no_upstream_account",
      groupName: opts.groupName ?? null,
    })
    return Response.json(
      {
        error: {
          message: `No usable ${opts.provider} account for this user`,
          type: "invalid_request_error",
          code: "no_upstream_account",
        },
      },
      { status: 400, headers: retryMarker(400, "no_upstream_account") },
    )
  }
  if (plan.kind === "unavailable") {
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider: opts.provider,
      model: canonicalModelId(opts.provider, opts.upstreamModel),
      statusCode: 503,
      latencyMs: Date.now() - started,
      errorCode: "upstream_unavailable",
      groupName: opts.groupName ?? null,
    })
    return upstreamUnavailableResponse(
      { error: { message: "All upstream accounts unavailable", code: "upstream_unavailable" } },
      plan.untilMs,
    )
  }

  let lastResponse: Response | null = null
  let lastCandidate: RoutingCandidate | null = null
  let lastUpstreamStatus: number | null = null
  let idx = 0
  let attempts = 0
  let ranOutOfCandidates = false

  for (; attempts < MAX_ATTEMPTS; ) {
    const candidate = plan.ordered[idx]
    if (!candidate) {
      ranOutOfCandidates = true
      break
    }
    idx++

    if (!candidate.adapter.audioTranscriptions) {
      continue
    }

    const acquired = await acquireCandidate(env, candidate)
    if (!acquired) continue
    attempts++

    let fetched: TimedUpstreamResponse
    try {
      fetched = await retry529Once(() =>
        fetchUpstreamResponse(env, (signal) =>
          candidate.adapter.audioTranscriptions!(
            env,
            acquired,
            opts.formData,
            opts.rawModel,
            candidate.upstreamModel,
            { signal },
          ),
        ),
      )
    } catch {
      const latencyMs = Date.now() - started
      await logRequest(env, {
        userId: opts.userId,
        apiKeyId: opts.apiKeyId,
        provider: candidate.provider,
        model: canonicalModelId(candidate.provider, candidate.upstreamModel),
        accountId: candidate.account.id,
        statusCode: 502,
        latencyMs,
        errorCode: "upstream_error",
        groupName: opts.groupName ?? null,
      })
      return Response.json(
        { error: { message: "upstream error", code: "upstream_error" } },
        { status: 502 },
      )
    }
    if (fetched.timedOut) continue
    const res = fetched.response
    lastUpstreamStatus = res.status
    opts.waitUntil(refreshAccountUsageInBackground(env, candidate.account, candidate.adapter))

    if (await shouldFailOverForResponse(env, opts.userId, candidate, res)) {
      if (lastResponse) {
        try {
          await lastResponse.body?.cancel()
        } catch {
          /* */
        }
      }
      lastResponse = res
      lastCandidate = candidate
      continue
    }

    const latencyMs = Date.now() - started
    const errorCode = res.ok ? null : "upstream_error"

    if (res.body && isEventStream(res)) {
      const sniffer = createOpenAISseUsageSniffer()
      const streamBody = streamWithKeepalive(res.body, undefined, {
        tap: (chunk) => sniffer.feed(chunk),
        idleTimeoutMs,
        stallFrame: OPENAI_STALL_FRAME,
        onClose: (reason) => {
          const usage = sniffer.finish()
          const code = errorCode ?? streamCloseErrorCode(reason, sniffer.complete())
          opts.waitUntil(
            logRequest(env, {
              userId: opts.userId,
              apiKeyId: opts.apiKeyId,
              provider: candidate.provider,
              model: canonicalModelId(candidate.provider, candidate.upstreamModel),
              accountId: candidate.account.id,
              statusCode: res.status,
              latencyMs,
              errorCode: code,
              upstreamStatus: res.status,
              groupName: opts.groupName ?? null,
              ...usageFields(usage),
            }),
          )
        },
      })
      return new Response(streamBody, {
        status: res.status,
        headers: passthroughStreamHeaders(res.headers),
      })
    }

    const ct = res.headers.get("content-type") || ""
    const isJson = ct.includes("application/json")

    const onFinish = (usage: NormalizedUsage | null, reason: StreamCloseReason) => {
      const code =
        reason === "cancel"
          ? "client_abort"
          : reason === "error"
            ? "upstream_error"
            : errorCode
      opts.waitUntil(
        logRequest(env, {
          userId: opts.userId,
          apiKeyId: opts.apiKeyId,
          provider: candidate.provider,
          model: canonicalModelId(candidate.provider, candidate.upstreamModel),
          accountId: candidate.account.id,
          statusCode: res.status,
          latencyMs,
          errorCode: code,
          upstreamStatus: res.status,
          groupName: opts.groupName ?? null,
          ...usageFields(usage),
        }),
      )
    }

    if (!res.body) {
      onFinish(null, "done")
      return res
    }

    const body = streamWithUsageTap(res.body, isJson, onFinish)
    return new Response(body, {
      status: res.status,
      statusText: res.statusText,
      headers: res.headers,
    })
  }

  if (ranOutOfCandidates && lastResponse && lastCandidate) {
    try {
      await lastResponse.body?.cancel()
    } catch {
      /* */
    }
    const untilMs = await recomputeUnavailableUntil(env, opts.userId, plan.ordered.slice(0, idx))
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider: lastCandidate.provider,
      model: canonicalModelId(lastCandidate.provider, lastCandidate.upstreamModel),
      accountId: lastCandidate.account.id,
      statusCode: 503,
      latencyMs: Date.now() - started,
      errorCode: "upstream_unavailable",
      upstreamStatus: lastUpstreamStatus,
      groupName: opts.groupName ?? null,
    })
    return upstreamUnavailableResponse(
      { error: { message: "All upstream accounts unavailable", code: "upstream_unavailable" } },
      untilMs,
    )
  }

  await logRequest(env, {
    userId: opts.userId,
    apiKeyId: opts.apiKeyId,
    provider: opts.provider,
    model: canonicalModelId(opts.provider, opts.upstreamModel),
    statusCode: 503,
    latencyMs: Date.now() - started,
    errorCode: "upstream_unavailable",
    upstreamStatus: lastUpstreamStatus,
    groupName: opts.groupName ?? null,
  })
  return upstreamUnavailableResponse(
    { error: { message: "All upstream accounts unavailable", code: "upstream_unavailable" } },
    null,
  )
}
