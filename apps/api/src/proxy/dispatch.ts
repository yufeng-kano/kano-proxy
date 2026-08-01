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

export async function dispatchAnthropicMessages(
  env: Env,
  opts: {
    userId: string
    apiKeyId: string | null
    body: unknown
    headers: Headers
    model: string
  },
): Promise<Response> {
  const started = Date.now()
  const adapter = getAdapter("claude-code")
  if (!adapter.messages) {
    return Response.json({ error: { message: "messages not supported" } }, { status: 500 })
  }
  const exclude = new Set<string>()
  let last: Response | null = null
  for (let i = 0; i < 8; i++) {
    const account = await acquireAccount(env, opts.userId, "claude-code", exclude)
    if (!account) {
      if (last) return last
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
    const res = await adapter.messages(env, refreshed, opts.body, opts.headers)
    if (shouldBenchStatus(res.status)) {
      await benchAccount(env, opts.userId, "claude-code", account.row.id)
      exclude.add(account.row.id)
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
