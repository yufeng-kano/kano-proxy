import { Hono } from "hono"
import { apiKeyAuth } from "../auth/api_key_auth"
import type { HonoEnv } from "../auth/session"
import { listModelsForUser } from "../catalog/models"
import type { ProviderId } from "../env"
import { logRequest } from "../logging/request_log"
import { resolveModel } from "../providers/resolve"
import { dispatchAnthropicMessages, dispatchAnthropicViaOpenAI } from "../proxy/dispatch"
import { detectAnthropicToolLoop, loopDetectedMessage } from "../utils/loop_guard"
import { loggingProviderFromRawModel } from "../utils/model"

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
  const started = Date.now()
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
        type: "error",
        error: {
          type: "invalid_request_error",
          message:
            "model must be provider/model (e.g. claude-code/claude-opus-5, grok/grok-4.5) or one of your model group names",
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

  // claude-code / custom anthropic-format: native Messages passthrough.
  // grok also exposes adapter.messages, but that path converts to Responses —
  // not Claude-native passthrough — so the loop guard still applies below.
  const isNativeAnthropicPassthrough =
    !!resolved.adapter.messages && resolved.provider !== "grok"

  if (isNativeAnthropicPassthrough) {
    // Only normalize model to the bare upstream id — strict cache_control
    // passthrough, do not touch other body fields.
    const upstreamBody = { ...body, model: resolved.upstreamModel }
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
      model: `${resolved.provider}/${resolved.upstreamModel}`,
      provider: resolved.provider,
      adapter: resolved.adapter,
      waitUntil: (p) => c.executionCtx.waitUntil(p),
      groupName: resolved.group?.name,
      pinnedAccountId: resolved.pinnedAccountId,
    })
  }

  // Conversion ingress (grok Responses / codex / custom-openai). Loop guard
  // applies here — never on the native passthrough branch above
  // (docs/api.md "Degenerate tool-call loop guard").
  const loop = detectAnthropicToolLoop((body.messages as unknown[]) ?? [])
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
        type: "error",
        error: { type: "invalid_request_error", message: loopDetectedMessage(loop) },
      },
      400,
    )
  }

  // grok: Anthropic ↔ Responses (encrypted reasoning). Not Chat Completions.
  if (resolved.provider === "grok" && resolved.adapter.messages) {
    const upstreamBody = { ...body, model: resolved.upstreamModel }
    const headers = new Headers()
    // Strip any client-supplied isolation headers; only the route may set them.
    const beta = c.req.header("anthropic-beta")
    const ver = c.req.header("anthropic-version")
    if (beta) headers.set("anthropic-beta", beta)
    if (ver) headers.set("anthropic-version", ver)
    if (affinity.convId) headers.set("x-grok-conv-id", affinity.convId)
    if (affinity.sessionId) headers.set("x-grok-session-id", affinity.sessionId)
    if (affinity.turnIdx) headers.set("x-grok-turn-idx", affinity.turnIdx)
    if (apiKeyId) headers.set("x-kano-api-key-id", apiKeyId)
    headers.set("x-kano-raw-model", resolved.raw)

    return dispatchAnthropicMessages(c.env, {
      userId,
      apiKeyId,
      body: upstreamBody,
      headers,
      model: `${resolved.provider}/${resolved.upstreamModel}`,
      provider: resolved.provider,
      adapter: resolved.adapter,
      waitUntil: (p) => c.executionCtx.waitUntil(p),
      groupName: resolved.group?.name,
      pinnedAccountId: resolved.pinnedAccountId,
    })
  }

  return dispatchAnthropicViaOpenAI(c.env, {
    userId,
    apiKeyId,
    provider: resolved.provider,
    adapter: resolved.adapter,
    rawModel: resolved.raw,
    upstreamModel: resolved.upstreamModel,
    body,
    affinity,
    waitUntil: (p) => c.executionCtx.waitUntil(p),
    groupName: resolved.group?.name,
    pinnedAccountId: resolved.pinnedAccountId,
  })
})

anthropicRoutes.post("/v1/messages/count_tokens", async (c) => {
  const started = Date.now()
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
        type: "error",
        error: {
          type: "invalid_request_error",
          message:
            "model must be provider/model (e.g. claude-code/claude-opus-5, grok/grok-4.5) or one of your model group names",
        },
      },
      400,
    )
  }

  // Builtins keep the exact prior provider-identity check (and its test);
  // custom providers are rejected the same way, by adapter capability —
  // custom anthropic-format has countTokens, custom openai-format does not.
  if (resolved.isBuiltin) {
    const rejection = countTokensProviderError(resolved.provider as ProviderId)
    if (rejection) return c.json(rejection, 400)
  } else if (!resolved.adapter.countTokens) {
    return c.json(COUNT_TOKENS_UNSUPPORTED, 400)
  }

  // Native passthrough only, same as /v1/messages: normalize model to bare
  // upstream id, forward anthropic-beta/anthropic-version if the client sent
  // them.
  const upstreamBody = { ...body, model: resolved.upstreamModel }
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
    model: `${resolved.provider}/${resolved.upstreamModel}`,
    provider: resolved.provider,
    adapter: resolved.adapter,
    endpoint: "count_tokens",
    waitUntil: (p) => c.executionCtx.waitUntil(p),
    groupName: resolved.group?.name,
    pinnedAccountId: resolved.pinnedAccountId,
  })
})

const COUNT_TOKENS_UNSUPPORTED = {
  type: "error",
  error: {
    type: "invalid_request_error",
    message: "count_tokens is only supported for claude-code models",
  },
} as const

/**
 * count_tokens has no grok/codex Chat Completions equivalent to convert to —
 * claude-code only. Exported as a pure function for unit testing.
 */
export function countTokensProviderError(provider: ProviderId): Record<string, unknown> | null {
  if (provider === "claude-code") return null
  return COUNT_TOKENS_UNSUPPORTED
}
