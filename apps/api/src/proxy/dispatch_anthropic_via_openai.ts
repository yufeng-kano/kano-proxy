/**
 * Anthropic Messages ingress for non-Claude providers: convert → OpenAI
 * chat path (`dispatchChatCompletions`, including its candidate walk) →
 * convert the response back. Strips cache_control on convert; never
 * invents Grok affinity ids (forward client headers via req.affinity only).
 */
import type { CustomProviderRow } from "../db/custom_providers"
import type { Env } from "../env"
import type { ChatCompletionRequest, ProviderAdapter } from "../providers/types"
import type { RoutingCandidate } from "../routing/types"
import { dispatchChatCompletions, isEventStream, passthroughStreamHeaders } from "./dispatch"
import type { WaitUntil } from "./dispatch_walk"
import { streamWithKeepalive } from "./sse"

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

