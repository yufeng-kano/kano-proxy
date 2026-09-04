import type { Context } from "hono"
import type { HonoEnv } from "../auth/session"
import { logRequest } from "../logging/request_log"
import { dispatchChatCompletions } from "../proxy/dispatch"
import {
  UnsupportedResponsesField,
  openaiSseToResponsesStream,
  openaiToResponsesObject,
  responsesToChatRequest,
  rewriteOpenAIErrorFramesToResponses,
} from "../proxy/responses_openai"
import { streamWithKeepalive } from "../proxy/sse"
import { resolveRequestModel } from "./resolve_request"
import { detectOpenAIToolLoop, loopDetectedMessage } from "../utils/loop_guard"
import { loggingProviderFromRawModel } from "../utils/model"
import { parseReasoningEffort } from "../utils/reasoning"

function isEventStream(res: Response): boolean {
  return (res.headers.get("content-type") || "").includes("text/event-stream")
}

function sseHeaders(): HeadersInit {
  return { "content-type": "text/event-stream; charset=utf-8", "cache-control": "no-cache" }
}

/**
 * `POST /openai/v1/responses` and its group mount (docs/api.md § `POST
 * /openai/v1/responses`). Native codex passthrough when every resolved
 * candidate is codex; otherwise Responses ↔ Chat conversion around the
 * ordinary chat dispatch. Pre-dispatch rejections mirror
 * `handleChatCompletions` — same codes, same logging, same retry marker.
 */
export async function handleResponses(c: Context<HonoEnv>): Promise<Response> {
  const started = Date.now()
  const userId = c.get("apiKeyUserId")!
  const apiKeyId = c.get("apiKeyId")
  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: { message: "Invalid JSON", type: "invalid_request_error", code: "invalid_request" } }, 400)
  }
  const modelRaw = String(body.model ?? "")

  const reject = (
    status: 400 | 404,
    code: string,
    message: string,
    target: { provider: string; model: string; groupName?: string | null },
  ) => {
    c.executionCtx.waitUntil(
      logRequest(c.env, {
        userId,
        apiKeyId,
        provider: target.provider,
        model: target.model.slice(0, 200),
        statusCode: status,
        latencyMs: Date.now() - started,
        errorCode: code,
        groupName: target.groupName ?? null,
      }),
    )
    return c.json(
      { error: { message, type: "invalid_request_error", code } },
      status,
      { "x-should-retry": "false" },
    )
  }

  const res = await resolveRequestModel(c, userId, modelRaw)
  if (res.kind !== "ok") {
    const target = { provider: loggingProviderFromRawModel(modelRaw), model: modelRaw }
    if (res.kind === "group_not_found") {
      return reject(404, "group_not_found", `unknown group endpoint "${res.slug}"`, target)
    }
    return reject(
      400,
      "invalid_model",
      res.groupSlug
        ? `model must be one of this group endpoint's configured models (see GET /g/${res.groupSlug}/openai/v1/models)`
        : "model must be provider/model (e.g. claude-code/claude-opus-5)",
      target,
    )
  }
  const resolved = res.resolution
  const target = {
    provider: resolved.primary.provider,
    model: `${resolved.primary.provider}/${resolved.primary.upstreamModel}`,
    groupName: resolved.groupName ?? null,
  }

  let converted: ReturnType<typeof responsesToChatRequest>
  try {
    converted = responsesToChatRequest(body)
  } catch (e) {
    if (e instanceof UnsupportedResponsesField) return reject(400, "unsupported_field", e.message, target)
    throw e
  }
  const chat = converted.chat

  const effort = parseReasoningEffort(chat.reasoning_effort)
  if (effort === "invalid") {
    return c.json(
      { error: { message: "invalid reasoning.effort", type: "invalid_request_error", code: "invalid_reasoning" } },
      400,
    )
  }

  const messages = chat.messages as unknown[]
  const loop = detectOpenAIToolLoop(messages)
  if (loop.tripped) return reject(400, "loop_detected", loopDetectedMessage(loop), target)

  // Native only when nothing in the candidate list could answer in another
  // wire: a mixed group's failover would otherwise hand a Responses client a
  // Chat stream (docs/api.md "Native path — every candidate is codex").
  const native =
    resolved.candidates.length > 0 && resolved.candidates.every((cand) => cand.provider === "codex")
  const stream = chat.stream === true
  const affinity = {
    convId: c.req.header("x-grok-conv-id"),
    sessionId: c.req.header("x-grok-session-id"),
    turnIdx: c.req.header("x-grok-turn-idx"),
  }

  const upstream = await dispatchChatCompletions(c.env, {
    userId,
    apiKeyId,
    provider: resolved.primary.provider,
    adapter: resolved.primary.adapter,
    waitUntil: (p) => c.executionCtx.waitUntil(p),
    groupName: resolved.groupName,
    candidates: resolved.candidates,
    strategy: resolved.strategy,
    isBuiltin: resolved.primary.isBuiltin,
    customProvider: resolved.primary.customProvider,
    req: {
      model: modelRaw,
      rawModel: modelRaw,
      upstreamModel: resolved.primary.upstreamModel,
      messages,
      stream,
      max_tokens: typeof chat.max_tokens === "number" ? chat.max_tokens : undefined,
      tools: chat.tools,
      tool_choice: chat.tool_choice,
      response_format: chat.response_format,
      reasoning_effort: effort,
      temperature: typeof chat.temperature === "number" ? chat.temperature : undefined,
      top_p: typeof chat.top_p === "number" ? chat.top_p : undefined,
      prompt_cache_key: typeof chat.prompt_cache_key === "string" ? chat.prompt_cache_key : undefined,
      affinity,
      rawBody: chat,
      ...(native ? { responsesBody: body } : {}),
    },
  })

  if (native) {
    // The codex adapter already answered in Responses shape; only dispatch's
    // own in-stream error frames need translating.
    if (upstream.body && (stream || isEventStream(upstream)) && upstream.ok) {
      return new Response(rewriteOpenAIErrorFramesToResponses(upstream.body, modelRaw), {
        status: upstream.status,
        headers: sseHeaders(),
      })
    }
    return upstream
  }

  const outputOpts = { model: modelRaw, toolNames: converted.toolNames }
  if (upstream.body && (stream || isEventStream(upstream))) {
    if (!upstream.ok) return upstream
    // Keepalive again on the converted stream: the inner wrapper's comments
    // are consumed by the converter, and a long reasoning turn would
    // otherwise send zero bytes until the first token.
    return new Response(streamWithKeepalive(openaiSseToResponsesStream(upstream.body, outputOpts)), {
      status: upstream.status,
      headers: sseHeaders(),
    })
  }
  const text = await upstream.text()
  if (!upstream.ok) {
    return new Response(text, {
      status: upstream.status,
      headers: {
        "content-type": upstream.headers.get("content-type") || "application/json",
        ...(upstream.headers.get("x-should-retry") ? { "x-should-retry": upstream.headers.get("x-should-retry")! } : {}),
        ...(upstream.headers.get("retry-after") ? { "retry-after": upstream.headers.get("retry-after")! } : {}),
      },
    })
  }
  try {
    const json = JSON.parse(text) as Record<string, unknown>
    return Response.json(openaiToResponsesObject(json, outputOpts))
  } catch {
    return new Response(text, {
      status: upstream.status,
      headers: { "content-type": upstream.headers.get("content-type") || "application/json" },
    })
  }
}
