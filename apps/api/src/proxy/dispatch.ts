import type { Env, ProviderId } from "../env"
import {
  acquireAccount,
  benchAccount,
  shouldBenchStatus,
  type AcquiredAccount,
} from "../pool/acquire"
import { getAdapter } from "../providers"
import type { ChatCompletionRequest } from "../providers/types"
import { logRequest } from "../logging/request_log"
import { streamWithKeepalive } from "./sse"

export async function dispatchChatCompletions(
  env: Env,
  opts: {
    userId: string
    apiKeyId: string | null
    provider: ProviderId
    req: ChatCompletionRequest & { rawModel: string }
  },
): Promise<Response> {
  const started = Date.now()
  const adapter = getAdapter(opts.provider)
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
    used = account
    let refreshed = account
    if (adapter.refreshIfNeeded) {
      refreshed = await adapter.refreshIfNeeded(env, account)
    }
    const res = await adapter.chatCompletions(env, refreshed, opts.req)
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
      const body = streamWithKeepalive(res.body)
      cExecutionLog(env, {
        userId: opts.userId,
        apiKeyId: opts.apiKeyId,
        provider: opts.provider,
        model: opts.req.rawModel,
        accountId: account.row.id,
        statusCode: res.status,
        latencyMs,
      })
      return new Response(body, {
        status: res.status,
        headers: passthroughStreamHeaders(res.headers),
      })
    }

    // clone for logging tokens if json
    const text = await res.text()
    let promptTokens: number | null = null
    let completionTokens: number | null = null
    try {
      const j = JSON.parse(text) as {
        usage?: { prompt_tokens?: number; completion_tokens?: number }
      }
      promptTokens = j.usage?.prompt_tokens ?? null
      completionTokens = j.usage?.completion_tokens ?? null
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
      promptTokens,
      completionTokens,
    })
    return new Response(text, {
      status: res.status,
      headers: { "content-type": res.headers.get("content-type") || "application/json" },
    })
  }

  await logRequest(env, {
    userId: opts.userId,
    apiKeyId: opts.apiKeyId,
    provider: opts.provider,
    model: opts.req.rawModel,
    statusCode: 503,
    latencyMs: Date.now() - started,
    errorCode: "upstream_unavailable",
  })
  return Response.json(
    {
      error: {
        message: "All upstream accounts unavailable",
        code: "upstream_unavailable",
      },
    },
    { status: 503 },
  )
}

/**
 * Native Anthropic Messages (or count_tokens) for claude-code only
 * (passthrough + auth). cache_control is never rewritten — body goes through
 * the adapter method as-is aside from fixed system prepend inside the
 * adapter. `endpoint` selects which adapter method carries the request;
 * both share this same bench/failover loop rather than duplicating it.
 */
export async function dispatchAnthropicMessages(
  env: Env,
  opts: {
    userId: string
    apiKeyId: string | null
    body: unknown
    headers: Headers
    model: string
    endpoint?: "messages" | "count_tokens"
  },
): Promise<Response> {
  const started = Date.now()
  const adapter = getAdapter("claude-code")
  const endpoint = opts.endpoint ?? "messages"
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
    const account = await acquireAccount(env, opts.userId, "claude-code", exclude)
    if (!account) {
      if (last) {
        await logRequest(env, {
          userId: opts.userId,
          apiKeyId: opts.apiKeyId,
          provider: "claude-code",
          model: opts.model,
          statusCode: last.status,
          latencyMs: Date.now() - started,
          errorCode: "upstream_error",
        })
        return last
      }
      return Response.json(
        {
          type: "error",
          error: {
            type: "invalid_request_error",
            message: "No usable claude-code account",
          },
        },
        { status: 400 },
      )
    }
    let refreshed = account
    if (adapter.refreshIfNeeded) refreshed = await adapter.refreshIfNeeded(env, account)
    const res = await call(env, refreshed, opts.body, opts.headers)
    if (shouldBenchStatus(res.status)) {
      await benchAccount(env, opts.userId, "claude-code", account.row.id)
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
    await logRequest(env, {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      provider: "claude-code",
      model: opts.model,
      accountId: account.row.id,
      statusCode: res.status,
      latencyMs: Date.now() - started,
    })
    if (res.body && isEventStream(res)) {
      return new Response(streamWithKeepalive(res.body), {
        status: res.status,
        headers: passthroughStreamHeaders(res.headers),
      })
    }
    return res
  }
  return Response.json(
    { type: "error", error: { type: "api_error", message: "upstream_unavailable" } },
    { status: 503 },
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
    provider: ProviderId
    rawModel: string
    upstreamModel: string
    body: Record<string, unknown>
    affinity?: ChatCompletionRequest["affinity"]
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
      stop: converted.stop,
      affinity: opts.affinity,
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

function cExecutionLog(
  env: Env,
  entry: Parameters<typeof logRequest>[1],
): void {
  // fire-and-forget style for streams
  void logRequest(env, entry)
}
