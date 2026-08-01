import { Hono } from "hono"
import { apiKeyAuth } from "../auth/api_key_auth"
import type { HonoEnv } from "../auth/session"
import { listModelsForUser } from "../catalog/models"
import { dispatchChatCompletions } from "../proxy/dispatch"
import { parseModelId } from "../utils/model"
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
  const parsed = parseModelId(modelRaw)
  if (!parsed) {
    return c.json(
      {
        error: {
          message: "model must be provider/model (e.g. claude-code/claude-opus-5)",
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

  // temperature intentionally stripped
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

  return dispatchChatCompletions(c.env, {
    userId,
    apiKeyId,
    provider: parsed.provider,
    req: {
      model: modelRaw,
      rawModel: modelRaw,
      upstreamModel: parsed.upstreamModel,
      messages: (body.messages as unknown[]) ?? [],
      stream: !!body.stream,
      max_tokens: maxTokens,
      tools: body.tools,
      tool_choice: body.tool_choice,
      response_format: body.response_format,
      reasoning_effort: effort,
      stop: stop.length ? stop : undefined,
      affinity: {
        convId: c.req.header("x-grok-conv-id"),
        sessionId: c.req.header("x-grok-session-id"),
        turnIdx: c.req.header("x-grok-turn-idx"),
      },
    },
  })
})
