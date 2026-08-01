import { Hono } from "hono"
import { apiKeyAuth } from "../auth/api_key_auth"
import type { HonoEnv } from "../auth/session"
import { listModelsForUser } from "../catalog/models"
import type { ProviderId } from "../env"
import {
  dispatchAnthropicMessages,
  dispatchAnthropicViaOpenAI,
} from "../proxy/dispatch"
import { parseAnthropicModel } from "../utils/model"

export const anthropicRoutes = new Hono<HonoEnv>()

anthropicRoutes.use("*", apiKeyAuth)

anthropicRoutes.get("/v1/models", async (c) => {
  const userId = c.get("apiKeyUserId")!
  const { models } = await listModelsForUser(c.env, userId, { availableOnly: true })
  // Same catalog as OpenAI surface; ids are always provider/upstream
  return c.json({
    data: models.map((m) => ({
      id: m.id,
      display_name: m.display_name,
      type: "model",
    })),
  })
})

anthropicRoutes.post("/v1/messages", async (c) => {
  const userId = c.get("apiKeyUserId")!
  const apiKeyId = c.get("apiKeyId")
  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json(
      { type: "error", error: { type: "invalid_request_error", message: "Invalid JSON" } },
      400,
    )
  }
  const modelRaw = String(body.model ?? "")
  const parsed = parseAnthropicModel(modelRaw)
  if (!parsed) {
    return c.json(
      {
        type: "error",
        error: {
          type: "invalid_request_error",
          message: "model must be provider/model (e.g. claude-code/claude-opus-5, grok/grok-4.5)",
        },
      },
      400,
    )
  }

  // Grok sticky headers: forward if client supplied; never invent
  const affinity = {
    convId: c.req.header("x-grok-conv-id") ?? undefined,
    sessionId: c.req.header("x-grok-session-id") ?? undefined,
    turnIdx: c.req.header("x-grok-turn-idx") ?? undefined,
  }

  if (parsed.provider === "claude-code") {
    // Native passthrough: only normalize model to bare upstream id.
    // Strict cache_control passthrough — do not touch other body fields.
    const upstreamBody = { ...body, model: parsed.upstreamModel }
    const headers = new Headers()
    const beta = c.req.header("anthropic-beta")
    const ver = c.req.header("anthropic-version")
    if (beta) headers.set("anthropic-beta", beta)
    if (ver) headers.set("anthropic-version", ver)

    return dispatchAnthropicMessages(c.env, {
      userId,
      apiKeyId,
      body: upstreamBody,
      headers,
      model: parsed.raw,
    })
  }

  // grok / codex: convert Messages ↔ Chat Completions via existing adapters
  return dispatchAnthropicViaOpenAI(c.env, {
    userId,
    apiKeyId,
    provider: parsed.provider,
    rawModel: parsed.raw,
    upstreamModel: parsed.upstreamModel,
    body,
    affinity,
  })
})

anthropicRoutes.post("/v1/messages/count_tokens", async (c) => {
  const userId = c.get("apiKeyUserId")!
  const apiKeyId = c.get("apiKeyId")
  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json(
      { type: "error", error: { type: "invalid_request_error", message: "Invalid JSON" } },
      400,
    )
  }
  const modelRaw = String(body.model ?? "")
  const parsed = parseAnthropicModel(modelRaw)
  if (!parsed) {
    return c.json(
      {
        type: "error",
        error: {
          type: "invalid_request_error",
          message: "model must be provider/model (e.g. claude-code/claude-opus-5, grok/grok-4.5)",
        },
      },
      400,
    )
  }

  const rejection = countTokensProviderError(parsed.provider)
  if (rejection) return c.json(rejection, 400)

  // Native passthrough only, same as /v1/messages: normalize model to bare
  // upstream id, forward anthropic-beta/anthropic-version if the client sent
  // them.
  const upstreamBody = { ...body, model: parsed.upstreamModel }
  const headers = new Headers()
  const beta = c.req.header("anthropic-beta")
  const ver = c.req.header("anthropic-version")
  if (beta) headers.set("anthropic-beta", beta)
  if (ver) headers.set("anthropic-version", ver)

  return dispatchAnthropicMessages(c.env, {
    userId,
    apiKeyId,
    body: upstreamBody,
    headers,
    model: parsed.raw,
    endpoint: "count_tokens",
  })
})

/**
 * count_tokens has no grok/codex Chat Completions equivalent to convert to —
 * claude-code only. Exported as a pure function for unit testing.
 */
export function countTokensProviderError(provider: ProviderId): Record<string, unknown> | null {
  if (provider === "claude-code") return null
  return {
    type: "error",
    error: {
      type: "invalid_request_error",
      message: "count_tokens is only supported for claude-code models",
    },
  }
}
