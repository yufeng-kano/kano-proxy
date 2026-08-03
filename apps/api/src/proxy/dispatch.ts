import type { Env, ProviderId } from "../env"
import {
  acquireAccount,
  benchAccount,
  shouldBenchStatus,
  type AcquiredAccount,
} from "../pool/acquire"
import { earliestBenchExpiry } from "../pool/bench"
import { listAccounts } from "../db/accounts"
import { getAdapter } from "../providers"
import type { ChatCompletionRequest, ProviderAdapter } from "../providers/types"
import { logRequest } from "../logging/request_log"
import {
  createAnthropicSseUsageSniffer,
  createOpenAISseUsageSniffer,
  fromAnthropicUsage,
  fromOpenAIUsage,
  type NormalizedUsage,
} from "../logging/usage_capture"
import { streamWithKeepalive, type StreamCloseReason } from "./sse"

/** Keeps a Worker invocation alive for a deferred `logRequest` past the returned Response — `c.executionCtx.waitUntil` in production, a test double in tests. */
export type WaitUntil = (promise: Promise<unknown>) => void

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

/** `Retry-After` header value (integer seconds, minimum 1) from a future epoch-ms bench-expiry. */
function retryAfterSeconds(untilMs: number): number {
  return Math.max(1, Math.ceil((untilMs - Date.now()) / 1000))
}

/**
 * Shared "pool unavailable" 503 for both surfaces — same envelope shape
 * either dispatch function already returned before `Retry-After` existed,
 * now with the header attached whenever the earliest bench expiry across
 * the affected accounts is known (docs/api.md "Errors"): the whole bound
 * pool is benched right now, or the 8-attempt failover loop exhausted it.
 */
function upstreamUnavailableResponse(body: Record<string, unknown>, untilMs: number | null): Response {
  const init: ResponseInit = { status: 503 }
  if (untilMs !== null) init.headers = { "retry-after": String(retryAfterSeconds(untilMs)) }
  return Response.json(body, init)
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
  },
): Promise<Response> {
  const started = Date.now()
  const adapter = opts.adapter ?? getAdapter(opts.provider as ProviderId)
  const idleTimeoutMs = opts.idleTimeoutMs ?? DEFAULT_IDLE_TIMEOUT_MS
  const exclude = new Set<string>()
  let lastResponse: Response | null = null
  let used: AcquiredAccount | null = null

  for (let attempt = 0; attempt < 8; attempt++) {
    const account = await acquireAccount(env, opts.userId, opts.provider, exclude)
    if (!account) {
      if (lastResponse) {
        await logRequest(env, {
          userId: opts.userId,
          apiKeyId: opts.apiKeyId,
          provider: opts.provider,
          model: opts.req.rawModel,
          accountId: used?.row.id,
          statusCode: lastResponse.status,
          latencyMs: Date.now() - started,
          errorCode: "upstream_error",
        })
        return lastResponse
      }
      // Distinguish "never bound" (fatal, 400) from "bound but every one is
      // benched or undecryptable right now" (transient, 503 + Retry-After)
      // — docs/api.md "Errors".
      const boundAccounts = await listAccounts(env.DB, opts.userId, opts.provider)
      if (boundAccounts.length === 0) {
        await logRequest(env, {
          userId: opts.userId,
          apiKeyId: opts.apiKeyId,
          provider: opts.provider,
          model: opts.req.rawModel,
          statusCode: 400,
          latencyMs: Date.now() - started,
          errorCode: "no_upstream_account",
        })
        return Response.json(
          {
            error: {
              message: `No usable ${opts.provider} account for this user`,
              type: "invalid_request_error",
              code: "no_upstream_account",
            },
          },
          { status: 400 },
        )
      }
      const untilMs = await earliestBenchExpiry(
        env,
        opts.userId,
        opts.provider,
        boundAccounts.map((a) => a.id),
      )
      await logRequest(env, {
        userId: opts.userId,
        apiKeyId: opts.apiKeyId,
        provider: opts.provider,
        model: opts.req.rawModel,
        statusCode: 503,
        latencyMs: Date.now() - started,
        errorCode: "upstream_unavailable",
      })
      return upstreamUnavailableResponse(
        { error: { message: "All upstream accounts unavailable", code: "upstream_unavailable" } },
        untilMs,
      )
    }
    used = account
    let refreshed = account
    if (adapter.refreshIfNeeded) {
      refreshed = await adapter.refreshIfNeeded(env, account)
    }
    const res = await adapter.chatCompletions(env, refreshed, opts.req, {
      apiKeyId: opts.apiKeyId,
      waitUntil: opts.waitUntil,
    })
    if (shouldBenchStatus(res.status)) {
      await benchAccount(env, opts.userId, opts.provider, account.row.id)
      exclude.add(account.row.id)
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

    const latencyMs = Date.now() - started
    if (res.body && (opts.req.stream || isEventStream(res))) {
      // Row is written once the stream ends (waitUntil), so token fields can
      // be populated — the sniffer taps the exact bytes piped to the client,
      // never buffering the stream itself.
      const sniffer = createOpenAISseUsageSniffer()
      const body = streamWithKeepalive(res.body, undefined, {
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
              provider: opts.provider,
              model: opts.req.rawModel,
              accountId: account.row.id,
              statusCode: res.status,
              latencyMs,
              errorCode,
              ...usageFields(usage),
            }),
          )
        },
      })
      return new Response(body, {
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
      provider: opts.provider,
      model: opts.req.rawModel,
      accountId: account.row.id,
      statusCode: res.status,
      latencyMs,
      ...usageFields(usage),
    })
    return new Response(text, {
      status: res.status,
      headers: { "content-type": res.headers.get("content-type") || "application/json" },
    })
  }

  // The loop itself benched every account it tried (each `continue` above
  // only follows a bench), so `exclude` is exactly that set — no extra D1
  // round-trip needed to compute Retry-After here.
  const untilMs = await earliestBenchExpiry(env, opts.userId, opts.provider, [...exclude])
  await logRequest(env, {
    userId: opts.userId,
    apiKeyId: opts.apiKeyId,
    provider: opts.provider,
    model: opts.req.rawModel,
    statusCode: 503,
    latencyMs: Date.now() - started,
    errorCode: "upstream_unavailable",
  })
  return upstreamUnavailableResponse(
    { error: { message: "All upstream accounts unavailable", code: "upstream_unavailable" } },
    untilMs,
  )
}

/**
 * Native Anthropic Messages (or count_tokens) passthrough — claude-code by
 * default, or a custom anthropic-format provider when `provider`/`adapter`
 * are given. cache_control is never rewritten — body goes through the
 * adapter method as-is aside from whatever fixed prepend that adapter itself
 * applies (claude-code only). `endpoint` selects which adapter method
 * carries the request; both share this same bench/failover loop rather than
 * duplicating it.
 */
export async function dispatchAnthropicMessages(
  env: Env,
  opts: {
    userId: string
    apiKeyId: string | null
    body: unknown
    headers: Headers
    model: string
    /** Builtin `ProviderId` or a custom provider's slug. Defaults to claude-code. */
    provider?: string
    /** Pre-resolved adapter for custom providers; defaults to the builtin registry. */
    adapter?: ProviderAdapter
    endpoint?: "messages" | "count_tokens"
    waitUntil: WaitUntil
    /** Testability hook for the streaming idle timeout; defaults to 120_000. */
    idleTimeoutMs?: number
  },
): Promise<Response> {
  const started = Date.now()
  const provider = opts.provider ?? "claude-code"
  const adapter = opts.adapter ?? getAdapter(provider as ProviderId)
  const endpoint = opts.endpoint ?? "messages"
  const idleTimeoutMs = opts.idleTimeoutMs ?? DEFAULT_IDLE_TIMEOUT_MS
  const call = endpoint === "count_tokens" ? adapter.countTokens : adapter.messages
  if (!call) {
    return Response.json(
      { type: "error", error: { type: "api_error", message: `${endpoint} not supported` } },
      { status: 500 },
    )
  }
  const exclude = new Set<string>()
  let last: Response | null = null
  for (let i = 0; i < 8; i++) {
    const account = await acquireAccount(env, opts.userId, provider, exclude)
    if (!account) {
      if (last) {
        await logRequest(env, {
          userId: opts.userId,
          apiKeyId: opts.apiKeyId,
          provider,
          model: opts.model,
          statusCode: last.status,
          latencyMs: Date.now() - started,
          errorCode: "upstream_error",
        })
        return last
      }
      // Same 400-vs-503 split as dispatchChatCompletions above.
      const boundAccounts = await listAccounts(env.DB, opts.userId, provider)
      if (boundAccounts.length === 0) {
        await logRequest(env, {
          userId: opts.userId,
          apiKeyId: opts.apiKeyId,
          provider,
          model: opts.model,
          statusCode: 400,
          latencyMs: Date.now() - started,
          errorCode: "no_upstream_account",
        })
        return Response.json(
          {
            type: "error",
            error: {
              type: "invalid_request_error",
              message: `No usable ${provider} account`,
            },
          },
          { status: 400 },
        )
      }
      const untilMs = await earliestBenchExpiry(
        env,
        opts.userId,
        provider,
        boundAccounts.map((a) => a.id),
      )
      await logRequest(env, {
        userId: opts.userId,
        apiKeyId: opts.apiKeyId,
        provider,
        model: opts.model,
        statusCode: 503,
        latencyMs: Date.now() - started,
        errorCode: "upstream_unavailable",
      })
      return upstreamUnavailableResponse(
        { type: "error", error: { type: "api_error", message: "upstream_unavailable" } },
        untilMs,
      )
    }
    let refreshed = account
    if (adapter.refreshIfNeeded) refreshed = await adapter.refreshIfNeeded(env, account)
    const res = await call(env, refreshed, opts.body, opts.headers, {
      waitUntil: opts.waitUntil,
    })
    if (shouldBenchStatus(res.status)) {
      await benchAccount(env, opts.userId, provider, account.row.id)
      exclude.add(account.row.id)
      if (last) {
        try {
          await last.body?.cancel()
        } catch {
          /* */
        }
      }
      last = res
      continue
    }
    const latencyMs = Date.now() - started
    // count_tokens estimates tokens, it never consumes them — never log a
    // count from that endpoint, streaming or not (it never streams anyway).
    const isCountTokens = endpoint === "count_tokens"
    if (res.body && isEventStream(res)) {
      const sniffer = createAnthropicSseUsageSniffer()
      const body = streamWithKeepalive(res.body, undefined, {
        tap: (chunk) => sniffer.feed(chunk),
        idleTimeoutMs,
        stallFrame: ANTHROPIC_STALL_FRAME,
        onClose: (reason) => {
          const usage = isCountTokens ? null : sniffer.finish()
          // count_tokens never streams in practice, but if it ever did, its
          // response isn't a message turn — completeness doesn't apply.
          const errorCode = isCountTokens
            ? null
            : streamCloseErrorCode(reason, sniffer.complete())
          opts.waitUntil(
            logRequest(env, {
              userId: opts.userId,
              apiKeyId: opts.apiKeyId,
              provider,
              model: opts.model,
              accountId: account.row.id,
              statusCode: res.status,
              latencyMs,
              errorCode,
              ...usageFields(usage),
            }),
          )
        },
      })
      return new Response(body, {
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
      provider,
      model: opts.model,
      accountId: account.row.id,
      statusCode: res.status,
      latencyMs,
      ...usageFields(usage),
    })
    return res
  }
  // Same "loop benched exactly `exclude`" reasoning as dispatchChatCompletions.
  const untilMs = await earliestBenchExpiry(env, opts.userId, provider, [...exclude])
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
  },
): Promise<Response> {
  const { anthropicToOpenAIChatRequest, openaiToAnthropicMessage, openaiSseToAnthropicStream } =
    await import("./openai_anthropic")
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
