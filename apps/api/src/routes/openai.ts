import { Hono } from "hono"
import { apiKeyAuth } from "../auth/api_key_auth"
import type { HonoEnv } from "../auth/session"
import { listModelsForUser } from "../catalog/models"
import { logRequest } from "../logging/request_log"
import { resolveModel } from "../providers/resolve"
import { dispatchChatCompletions } from "../proxy/dispatch"
import { detectOpenAIToolLoop, loopDetectedMessage } from "../utils/loop_guard"
import { loggingProviderFromRawModel } from "../utils/model"
import { parseReasoningEffort } from "../utils/reasoning"

export const openaiRoutes = new Hono<HonoEnv>()

openaiRoutes.use("*", apiKeyAuth)

openaiRoutes.get("/models", async (c) => {
  const userId = c.get("apiKeyUserId")!
  // Live upstream models for providers the key owner has accounts for
  const { models } = await listModelsForUser(c.env, userId, { availableOnly: true })
  return c.json({
    object: "list",
    data: models.map((m) => ({
      id: m.id,
      object: "model",
      owned_by: m.owned_by,
      display_name: m.display_name,
    })),
  })
})

openaiRoutes.post("/chat/completions", async (c) => {
  const started = Date.now()
  const userId = c.get("apiKeyUserId")!
  const apiKeyId = c.get("apiKeyId")
  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json(
      { error: { message: "Invalid JSON", code: "invalid_request" } },
      400,
    )
  }
  const modelRaw = String(body.model ?? "")
  const resolved = await resolveModel(c.env, userId, modelRaw)
  if (!resolved) {
    c.executionCtx.waitUntil(
      logRequest(c.env, {
        userId,
        apiKeyId,
        provider: loggingProviderFromRawModel(modelRaw),
        model: modelRaw.slice(0, 200),
        statusCode: 400,
        latencyMs: Date.now() - started,
        errorCode: "invalid_model",
      }),
    )
    return c.json(
      {
        error: {
          message:
            "model must be provider/model (e.g. claude-code/claude-opus-5) or one of your model group names",
          code: "invalid_model",
        },
      },
      400,
    )
  }
  const effort = parseReasoningEffort(body.reasoning_effort)
  if (effort === "invalid") {
    return c.json(
      { error: { message: "invalid reasoning_effort", code: "invalid_reasoning" } },
      400,
    )
  }

  // Loop guard applies on conversion ingress (grok / codex / custom-openai);
  // never on native Anthropic passthrough adapters (claude-code /
  // custom-anthropic). grok exposes `messages()` for the /anthropic →
  // Responses path, but /openai/v1 still uses chatCompletions — keep it
  // guarded (docs/api.md "Degenerate tool-call loop guard").
  const nativeAnthropicPassthrough =
    !!resolved.adapter.messages && resolved.provider !== "grok"
  if (!nativeAnthropicPassthrough) {
    const loop = detectOpenAIToolLoop((body.messages as unknown[]) ?? [])
    if (loop.tripped) {
      c.executionCtx.waitUntil(
        logRequest(c.env, {
          userId,
          apiKeyId,
          provider: resolved.provider,
          model: `${resolved.provider}/${resolved.upstreamModel}`,
          statusCode: 400,
          latencyMs: Date.now() - started,
          errorCode: "loop_detected",
          groupName: resolved.group?.name ?? null,
        }),
      )
      return c.json(
        {
          error: {
            message: loopDetectedMessage(loop),
            type: "invalid_request_error",
            code: "loop_detected",
          },
        },
        400,
      )
    }
  }

  // temperature / top_p: read numeric values verbatim. Built-in adapters
  // decide per-provider what to do with them (grok forwards/defaults it,
  // claude-code clamps it, codex ignores it — see providers/*.ts); a
  // custom-openai provider forwards it verbatim via `rawBody` regardless.
  const temperature = typeof body.temperature === "number" ? body.temperature : undefined
  const topP = typeof body.top_p === "number" ? body.top_p : undefined
  const stopRaw = Array.isArray(body.stop)
    ? body.stop
    : typeof body.stop === "string"
      ? [body.stop]
      : []
  const stop = stopRaw.filter(
    (s): s is string => typeof s === "string" && s.length > 0,
  )
  const maxTokens =
    typeof body.max_tokens === "number"
      ? body.max_tokens
      : typeof body.max_completion_tokens === "number"
        ? body.max_completion_tokens
        : undefined
  const promptCacheKey =
    typeof body.prompt_cache_key === "string" && body.prompt_cache_key
      ? body.prompt_cache_key
      : undefined

  return dispatchChatCompletions(c.env, {
    userId,
    apiKeyId,
    provider: resolved.provider,
    adapter: resolved.adapter,
    waitUntil: (p) => c.executionCtx.waitUntil(p),
    groupName: resolved.group?.name,
    pinnedAccountId: resolved.pinnedAccountId,
    req: {
      model: modelRaw,
      rawModel: modelRaw,
      upstreamModel: resolved.upstreamModel,
      messages: (body.messages as unknown[]) ?? [],
      stream: !!body.stream,
      max_tokens: maxTokens,
      tools: body.tools,
      tool_choice: body.tool_choice,
      response_format: body.response_format,
      reasoning_effort: effort,
      temperature,
      top_p: topP,
      stop: stop.length ? stop : undefined,
      prompt_cache_key: promptCacheKey,
      affinity: {
        convId: c.req.header("x-grok-conv-id"),
        sessionId: c.req.header("x-grok-session-id"),
        turnIdx: c.req.header("x-grok-turn-idx"),
      },
      // Raw client body for the custom-openai passthrough adapter; ignored
      // by built-ins, which build their upstream body from named fields.
      rawBody: body,
    },
  })
})
