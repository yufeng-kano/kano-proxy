import { Hono } from "hono"
import { apiKeyAuth } from "../auth/api_key_auth"
import type { HonoEnv } from "../auth/session"
import { listModelsForUser } from "../catalog/models"
import type { ProviderId } from "../env"
import { logRequest } from "../logging/request_log"
import { resolveCandidates } from "../routing/candidates"
import { relayCountTokens } from "../providers/codex_count"
import { dispatchAnthropicMessages, dispatchAnthropicViaOpenAI } from "../proxy/dispatch"
import { detectAnthropicToolLoop, loopDetectedMessage } from "../utils/loop_guard"
import { loggingProviderFromRawModel } from "../utils/model"

/**
 * Builtins that expose `adapter.messages` but **convert** to a non-Anthropic
 * upstream format rather than passing the body through: grok → xAI Responses,
 * antigravity → Gemini `GenerateContent`. Everything else with a `messages()`
 * (claude-code, custom anthropic-format) is a true native passthrough, which
 * is what decides `cache_control` handling and whether the tool-loop guard
 * runs (docs/api.md "Degenerate tool-call loop guard").
 */
const CONVERTING_MESSAGES_PROVIDERS: ReadonlySet<string> = new Set(["grok", "antigravity"])

export function isNativeAnthropicPassthrough(
  provider: string,
  hasMessages: boolean,
): boolean {
  return hasMessages && !CONVERTING_MESSAGES_PROVIDERS.has(provider)
}

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
  const resolved = await resolveCandidates(c.env, userId, modelRaw)
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
            "model must be provider/model (e.g. claude-code/claude-opus-5, grok/grok-4.5) or one of your model group aliases",
        },
      },
      400,
      { "x-should-retry": "false" },
    )
  }

  // Grok sticky headers: forward if client supplied; never invent
  const affinity = {
    convId: c.req.header("x-grok-conv-id") ?? undefined,
    sessionId: c.req.header("x-grok-session-id") ?? undefined,
    turnIdx: c.req.header("x-grok-turn-idx") ?? undefined,
  }

  // claude-code / custom anthropic-format: native Messages passthrough.
  // grok and antigravity also expose adapter.messages, but those paths convert
  // to another wire format, so the loop guard still applies below.
  // Decided from the highest-priority resolved target only — a structural,
  // not usability-based, property (routing/candidates.ts `primary`); a
  // later, differently-shaped target within the same group simply can't be
  // reached by this call shape (docs/providers.md § Routing module).
  const nativePassthrough = isNativeAnthropicPassthrough(
    resolved.primary.provider,
    !!resolved.primary.adapter.messages,
  )

  if (nativePassthrough) {
    // Only normalize model to the bare upstream id — strict cache_control
    // passthrough, do not touch other body fields.
    const upstreamBody = { ...body, model: resolved.primary.upstreamModel }
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
      model: `${resolved.primary.provider}/${resolved.primary.upstreamModel}`,
      provider: resolved.primary.provider,
      adapter: resolved.primary.adapter,
      waitUntil: (p) => c.executionCtx.waitUntil(p),
      groupName: resolved.groupName,
      candidates: resolved.candidates,
      strategy: resolved.strategy,
      isBuiltin: resolved.primary.isBuiltin,
      customProvider: resolved.primary.customProvider,
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
        provider: resolved.primary.provider,
        model: `${resolved.primary.provider}/${resolved.primary.upstreamModel}`,
        statusCode: 400,
        latencyMs: Date.now() - started,
        errorCode: "loop_detected",
        groupName: resolved.groupName ?? null,
      }),
    )
    return c.json(
      {
        type: "error",
        error: { type: "invalid_request_error", message: loopDetectedMessage(loop) },
      },
      400,
      { "x-should-retry": "false" },
    )
  }

  // grok (Anthropic ↔ xAI Responses, encrypted reasoning) and antigravity
  // (Anthropic ↔ Gemini) both convert inside their own `messages()`, so they
  // dispatch through the Messages path rather than the Chat Completions one.
  if (
    CONVERTING_MESSAGES_PROVIDERS.has(resolved.primary.provider) &&
    resolved.primary.adapter.messages
  ) {
    const upstreamBody = { ...body, model: resolved.primary.upstreamModel }
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
      model: `${resolved.primary.provider}/${resolved.primary.upstreamModel}`,
      provider: resolved.primary.provider,
      adapter: resolved.primary.adapter,
      waitUntil: (p) => c.executionCtx.waitUntil(p),
      groupName: resolved.groupName,
      candidates: resolved.candidates,
      strategy: resolved.strategy,
      isBuiltin: resolved.primary.isBuiltin,
      customProvider: resolved.primary.customProvider,
    })
  }

  return dispatchAnthropicViaOpenAI(c.env, {
    userId,
    apiKeyId,
    provider: resolved.primary.provider,
    adapter: resolved.primary.adapter,
    rawModel: resolved.raw,
    upstreamModel: resolved.primary.upstreamModel,
    body,
    affinity,
    waitUntil: (p) => c.executionCtx.waitUntil(p),
    groupName: resolved.groupName,
    candidates: resolved.candidates,
    strategy: resolved.strategy,
    isBuiltin: resolved.primary.isBuiltin,
    customProvider: resolved.primary.customProvider,
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
  const resolved = await resolveCandidates(c.env, userId, modelRaw)
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
            "model must be provider/model (e.g. claude-code/claude-opus-5, grok/grok-4.5) or one of your model group aliases",
        },
      },
      400,
    )
  }

  // Providers with no upstream counting endpoint are answered locally —
  // codex via the relay tokenizer, grok and url-less custom-openai with the
  // sentinel zero. Never a 400: a failed count_tokens sends Claude Code into
  // a parallel max_tokens:1 probe burst against the real upstream
  // (docs/api.md § count_tokens, measured 2026-08-22).
  const localMode = resolved.primary.isBuiltin
    ? countTokensLocalMode(resolved.primary.provider as ProviderId)
    : resolved.primary.adapter.countTokens
      ? null
      : "stub"
  if (localMode) {
    const tokens = localMode === "relay" ? await relayCountTokens(c.env, body) : null
    c.executionCtx.waitUntil(
      logRequest(c.env, {
        userId,
        apiKeyId,
        provider: resolved.primary.provider,
        model: `${resolved.primary.provider}/${resolved.primary.upstreamModel}`,
        statusCode: 200,
        latencyMs: Date.now() - started,
        errorCode: tokens === null ? "count_tokens_stub" : null,
        groupName: resolved.groupName ?? null,
      }),
    )
    return c.json({ input_tokens: tokens ?? 0 })
  }

  // Native passthrough only, same as /v1/messages: normalize model to bare
  // upstream id, forward anthropic-beta/anthropic-version if the client sent
  // them.
  const upstreamBody = { ...body, model: resolved.primary.upstreamModel }
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
    model: `${resolved.primary.provider}/${resolved.primary.upstreamModel}`,
    provider: resolved.primary.provider,
    adapter: resolved.primary.adapter,
    endpoint: "count_tokens",
    waitUntil: (p) => c.executionCtx.waitUntil(p),
    groupName: resolved.groupName,
    candidates: resolved.candidates,
    strategy: resolved.strategy,
    isBuiltin: resolved.primary.isBuiltin,
    customProvider: resolved.primary.customProvider,
  })
})

/**
 * How count_tokens answers when the provider has no upstream counting
 * endpoint: `"relay"` = the egress relay's local o200k_base tokenizer
 * (codex), `"stub"` = the sentinel `{input_tokens: 0}` (grok), `null` = a
 * real upstream count exists and dispatch handles it (claude-code forwards
 * natively, antigravity calls `v1internal:countTokens`). Exported as a pure
 * function for unit testing.
 */
export function countTokensLocalMode(provider: ProviderId): "relay" | "stub" | null {
  if (provider === "codex") return "relay"
  if (provider === "grok") return "stub"
  return null
}
