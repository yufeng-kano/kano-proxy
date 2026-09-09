/**
 * Chat Completions and Anthropic Messages dispatch: two transports (eager
 * streaming commit, non-stream) over the single candidate walk in
 * `dispatch_walk.ts`, generic over a `Wire` for everything protocol-specific
 * (docs/project-structure.md § Boundaries, docs/api.md "Eager streaming commit").
 */
import type { CustomProviderRow } from "../db/custom_providers"
import type { Env, ProviderId } from "../env"
import { getAdapter } from "../providers"
import type { ChatCompletionRequest, ProviderAdapter } from "../providers/types"
import type { RoutingCandidate } from "../routing/types"
import { logRequest } from "../logging/request_log"
import type { NormalizedUsage } from "../logging/usage_capture"
import { splitModelId } from "../utils/model"
import {
  recomputeUnavailableUntil,
  walkCandidates,
  type CandidateSource,
  type UpstreamCall,
  type WaitUntil,
  type WalkProgress,
} from "./dispatch_walk"
import { streamWithEagerProducer, streamWithKeepalive, type StreamCloseReason } from "./sse"
import type { Wire } from "./wire"
import { anthropicWire } from "./wire_anthropic"
import { openaiWire } from "./wire_openai"

export type { WaitUntil } from "./dispatch_walk"

/**
 * `request_logs.model`/`provider` always store the expanded canonical
 * target, never a model-group alias — reconstructed the same way
 * `splitModelId` builds `raw` (prefix + "/" + rest), so this is
 * byte-identical to `req.rawModel` whenever the client sent a direct
 * `provider/model` id and only diverges when a group expanded it
 * (docs/api.md "Model routing").
 */
export function canonicalModelId(provider: string, upstreamModel: string): string {
  return `${provider}/${upstreamModel}`
}

/** No real upstream chunk for this long tears the stream down — docs/api.md "Streaming". */
export const DEFAULT_IDLE_TIMEOUT_MS = 120_000

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
export function streamCloseErrorCode(reason: StreamCloseReason, complete: boolean): string | null {
  if (reason === "idle_timeout") return "upstream_stall"
  if (complete) return null
  if (reason === "cancel") return "client_abort"
  return "incomplete_stream"
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

/**
 * Shared "pool unavailable" 503 for both surfaces, with `Retry-After`
 * attached whenever the earliest bench/limit expiry across the candidates
 * is known (docs/api.md "Errors"): every candidate is currently unusable,
 * or the 8-attempt walk exhausted the ones that were.
 */
function upstreamUnavailableResponse(body: Record<string, unknown>, untilMs: number | null): Response {
  const headers = new Headers(retryMarker(503, "upstream_unavailable"))
  if (untilMs !== null) headers.set("retry-after", String(retryAfterSeconds(untilMs)))
  return Response.json(body, { status: 503, headers })
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

/** SSE response headers for eager commit (HTTP already 200 before upstream). */
function sseResponseHeaders(): Headers {
  const out = new Headers()
  out.set("content-type", "text/event-stream; charset=utf-8")
  out.set("cache-control", "no-cache")
  return out
}

export function isEventStream(res: Response): boolean {
  const ct = res.headers.get("content-type") || ""
  return ct.includes("text/event-stream")
}

export function passthroughStreamHeaders(h: Headers): Headers {
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
export function usageFields(usage: NormalizedUsage | null): {
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

/** What a transport needs beyond the walk itself: where to log, which protocol, and how to reach the upstream per candidate. */
export type TransportOpts = {
  userId: string
  apiKeyId: string | null
  /** The model-group alias this request was addressed to, if any (docs/database.md `request_logs.group_name`). */
  groupName?: string
  waitUntil: WaitUntil
  /** Testability hook for the streaming idle timeout; defaults to 120_000. */
  idleTimeoutMs?: number
  /** Logged on rows written before any candidate was attempted (`no_upstream_account`, pool unavailable, attempt cap). `model` is already canonical. */
  requested: { provider: string; model: string }
  source: CandidateSource
  upstreamModel: string
  callFor: (candidate: RoutingCandidate) => UpstreamCall | null
  wire: Wire
  /** `false` for `count_tokens`: an estimate never consumes tokens, so no usage is ever logged from it. */
  captureUsage: boolean
}

type LogRow = Omit<Parameters<typeof logRequest>[1], "userId" | "apiKeyId" | "groupName">

function requestedRow(t: TransportOpts): { provider: string; model: string } {
  return { provider: t.requested.provider, model: t.requested.model }
}

function candidateRow(c: RoutingCandidate): { provider: string; model: string; accountId: string } {
  return { provider: c.provider, model: canonicalModelId(c.provider, c.upstreamModel), accountId: c.account.id }
}

/**
 * Eager streaming commit: return 200 + SSE immediately, run the candidate
 * walk inside the stream (docs/api.md "Eager streaming commit"). Every
 * failure after commit is a terminal frame; one log row on close.
 */
async function dispatchEager(env: Env, t: TransportOpts): Promise<Response> {
  const started = Date.now()
  const idleTimeoutMs = t.idleTimeoutMs ?? DEFAULT_IDLE_TIMEOUT_MS
  const sniffer = t.wire.createUsageSniffer()
  const progress: WalkProgress = { candidate: null, upstreamStatus: null }
  let forcedErrorCode: string | null = null
  /** TTFB into the pipe, or time until terminal fail/cancel if earlier. */
  let headersLatencyMs: number | null = null

  const body = streamWithEagerProducer(
    async (ctl) => {
      const fail = (errorCode: string, frame: Uint8Array) => {
        headersLatencyMs = Date.now() - started
        forcedErrorCode = errorCode
        ctl.fail(frame)
      }
      const outcome = await walkCandidates(env, { ...t, cancelled: ctl.cancelled, progress })
      switch (outcome.kind) {
        case "cancelled":
          return
        case "no_account":
          return fail("no_upstream_account", t.wire.noAccountFrame(t.requested.provider))
        case "unavailable":
        case "exhausted":
          return fail("upstream_unavailable", t.wire.unavailableFrame())
        case "fetch_error":
          return fail("upstream_error", t.wire.upstreamErrorFrame())
        case "response":
          break
      }

      const res = outcome.response
      if (res.body && res.ok && isEventStream(res)) {
        headersLatencyMs = Date.now() - started
        await ctl.pipeUpstream(res.body, {
          tap: (chunk) => sniffer.feed(chunk),
          idleTimeoutMs,
          stallFrame: t.wire.stallFrame,
        })
        return
      }

      const text = await safeResponseText(res)
      if (!res.ok) {
        return fail(
          "upstream_error",
          t.wire.upstreamErrorFrameFromBody(messageFromUpstreamErrorBody(text, "upstream error"), text),
        )
      }
      // 200 but not event-stream under stream:true — nothing useful to pipe.
      headersLatencyMs = Date.now() - started
      ctl.close()
    },
    undefined,
    {
      errorFrame: t.wire.upstreamErrorFrame(),
      onClose: (reason) => {
        const usage = sniffer.finish()
        const errorCode = forcedErrorCode ?? streamCloseErrorCode(reason, sniffer.complete())
        t.waitUntil(
          logRequest(env, {
            userId: t.userId,
            apiKeyId: t.apiKeyId,
            groupName: t.groupName ?? null,
            ...(progress.candidate ? candidateRow(progress.candidate) : requestedRow(t)),
            statusCode: 200,
            latencyMs: headersLatencyMs ?? Date.now() - started,
            errorCode,
            upstreamStatus: progress.upstreamStatus,
            ...usageFields(usage),
          }),
        )
      },
    },
  )

  return new Response(body, { status: 200, headers: sseResponseHeaders() })
}

/**
 * Non-stream transport: run the walk first, then mirror the outcome as a
 * real HTTP status — `400` / `503` (+ `Retry-After`) / `502` for pool and
 * fetch failures, upstream status passed through otherwise (docs/api.md
 * "Errors"). `deliver` shapes the response for a non-bench upstream
 * response; the default below serves Chat Completions and Messages, audio
 * supplies its own.
 */
export async function dispatchNonStream(
  env: Env,
  t: TransportOpts,
  deliver: (candidate: RoutingCandidate, res: Response, latencyMs: number) => Promise<Response> = (c, res, ms) =>
    deliverNonStream(env, t, c, res, ms),
): Promise<Response> {
  const started = Date.now()
  const progress: WalkProgress = { candidate: null, upstreamStatus: null }
  const log = (row: LogRow) =>
    logRequest(env, { userId: t.userId, apiKeyId: t.apiKeyId, groupName: t.groupName ?? null, ...row })

  const outcome = await walkCandidates(env, { ...t, progress })
  switch (outcome.kind) {
    case "cancelled":
      throw new Error("candidate walk reported cancelled without a cancel signal")
    case "no_account":
      await log({ ...requestedRow(t), statusCode: 400, latencyMs: Date.now() - started, errorCode: "no_upstream_account" })
      return Response.json(t.wire.noAccountBody(t.requested.provider), {
        status: 400,
        headers: retryMarker(400, "no_upstream_account"),
      })
    case "unavailable":
      await log({ ...requestedRow(t), statusCode: 503, latencyMs: Date.now() - started, errorCode: "upstream_unavailable" })
      return upstreamUnavailableResponse(t.wire.unavailableBody(), outcome.untilMs)
    case "exhausted": {
      // Same 503 + Retry-After as the all-unusable case, recomputed now that
      // this walk just benched more of the pool.
      const untilMs = await recomputeUnavailableUntil(env, t.userId, outcome.tried)
      await log({
        ...(outcome.lastBenched ? candidateRow(outcome.lastBenched) : requestedRow(t)),
        statusCode: 503,
        latencyMs: Date.now() - started,
        errorCode: "upstream_unavailable",
        upstreamStatus: progress.upstreamStatus,
      })
      return upstreamUnavailableResponse(t.wire.unavailableBody(), untilMs)
    }
    case "fetch_error":
      await log({
        ...candidateRow(outcome.candidate),
        statusCode: 502,
        latencyMs: Date.now() - started,
        errorCode: "upstream_error",
      })
      return Response.json(t.wire.upstreamErrorBody(), { status: 502 })
    case "response":
      return deliver(outcome.candidate, outcome.response, Date.now() - started)
  }
}

/**
 * Default non-stream delivery. An event-stream body under a non-stream
 * request ("legacy attach", docs/logging.md) gets keepalive + idle timeout
 * and the upstream status; anything else is returned as received — the
 * usage peek reads a clone so every upstream header still passes through.
 */
async function deliverNonStream(
  env: Env,
  t: TransportOpts,
  candidate: RoutingCandidate,
  res: Response,
  latencyMs: number,
): Promise<Response> {
  const row = candidateRow(candidate)
  const log = (fields: { errorCode?: string | null; usage: NormalizedUsage | null }) =>
    logRequest(env, {
      userId: t.userId,
      apiKeyId: t.apiKeyId,
      groupName: t.groupName ?? null,
      ...row,
      statusCode: res.status,
      latencyMs,
      errorCode: fields.errorCode,
      upstreamStatus: res.status,
      ...usageFields(fields.usage),
    })

  if (res.body && isEventStream(res)) {
    const sniffer = t.wire.createUsageSniffer()
    const streamBody = streamWithKeepalive(res.body, undefined, {
      tap: (chunk) => sniffer.feed(chunk),
      idleTimeoutMs: t.idleTimeoutMs ?? DEFAULT_IDLE_TIMEOUT_MS,
      stallFrame: t.wire.stallFrame,
      onClose: (reason) => {
        t.waitUntil(
          log({
            usage: t.captureUsage ? sniffer.finish() : null,
            errorCode: t.captureUsage ? streamCloseErrorCode(reason, sniffer.complete()) : null,
          }),
        )
      },
    })
    return new Response(streamBody, { status: res.status, headers: passthroughStreamHeaders(res.headers) })
  }

  let usage: NormalizedUsage | null = null
  if (t.captureUsage) {
    try {
      const json = (await res.clone().json()) as { usage?: Record<string, unknown> }
      usage = t.wire.parseUsage(json.usage)
    } catch {
      /* not JSON, or no body — keep NULL usage */
    }
  }
  await log({ usage })
  return res
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
  const transport: TransportOpts = {
    userId: opts.userId,
    apiKeyId: opts.apiKeyId,
    groupName: opts.groupName,
    waitUntil: opts.waitUntil,
    idleTimeoutMs: opts.idleTimeoutMs,
    requested: { provider: opts.provider, model: canonicalModelId(opts.provider, opts.req.upstreamModel) },
    source: opts,
    upstreamModel: opts.req.upstreamModel,
    callFor: (candidate) => (acquired, signal) =>
      candidate.adapter.chatCompletions(
        env,
        acquired,
        { ...opts.req, upstreamModel: candidate.upstreamModel },
        { apiKeyId: opts.apiKeyId, waitUntil: opts.waitUntil, signal },
      ),
    wire: openaiWire,
    captureUsage: true,
  }
  return opts.req.stream ? dispatchEager(env, transport) : dispatchNonStream(env, transport)
}

function bodyWantsStream(body: unknown): boolean {
  return !!body && typeof body === "object" && (body as { stream?: unknown }).stream === true
}

/**
 * Native Anthropic Messages (or count_tokens) passthrough — claude-code by
 * default, or a custom anthropic-format provider when `provider`/`adapter`
 * are given. cache_control is never rewritten — body goes through the
 * adapter method as-is aside from whatever fixed prepend that adapter itself
 * applies (claude-code only) and the `model` rewrite to the acquired
 * candidate's own upstream id. `endpoint` selects which adapter method
 * carries the request; both share the same candidate walk.
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
  const provider = opts.provider ?? "claude-code"
  const endpoint = opts.endpoint ?? "messages"

  // Fixed adapter (no candidates): keep the old immediate rejection for an
  // adapter that never supports this endpoint at all, before ever touching
  // the pool — 500, not a synthesized pool-exhaustion error.
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

  const transport: TransportOpts = {
    userId: opts.userId,
    apiKeyId: opts.apiKeyId,
    groupName: opts.groupName,
    waitUntil: opts.waitUntil,
    idleTimeoutMs: opts.idleTimeoutMs,
    requested: { provider, model: opts.model },
    source: { ...opts, provider },
    // Bare upstream id from the canonical `provider/upstreamModel` string — builds the single-pool candidate when no pre-built list was handed in.
    upstreamModel: splitModelId(opts.model)?.upstreamModel ?? opts.model,
    callFor: (candidate) => {
      const call = endpoint === "count_tokens" ? candidate.adapter.countTokens : candidate.adapter.messages
      if (!call) return null
      // `body` with `.model` rewritten to the candidate's own upstream id — cache_control and everything else pass through untouched.
      const body = { ...(opts.body as Record<string, unknown>), model: candidate.upstreamModel }
      return (acquired, signal) => call(env, acquired, body, opts.headers, { waitUntil: opts.waitUntil, signal })
    },
    wire: anthropicWire,
    captureUsage: endpoint !== "count_tokens",
  }
  return endpoint === "messages" && bodyWantsStream(opts.body)
    ? dispatchEager(env, transport)
    : dispatchNonStream(env, transport)
}
