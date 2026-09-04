import { Hono } from "hono"
import type { Context } from "hono"
import { apiKeyAuth } from "../auth/api_key_auth"
import type { HonoEnv } from "../auth/session"
import { listModelsForUser } from "../catalog/models"
import { logRequest } from "../logging/request_log"
import { isNativeAnthropicPassthrough } from "./anthropic"
import { resolveRequestModel } from "./resolve_request"
import { handleResponses } from "./responses"
import { dispatchAudioTranscriptions, dispatchChatCompletions } from "../proxy/dispatch"
import { SUPPORTED_AUDIO_FORMATS, scanAudioParts } from "../utils/audio"
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

/**
 * Shared by the shared base (`/openai/v1/chat/completions`) and the group
 * mounts (`/g/:slug/openai/v1/chat/completions`) — resolution branches on the
 * `slug` param inside `resolveRequestModel` (docs/api.md § Group endpoints);
 * everything past resolution is identical.
 */
export async function handleChatCompletions(c: Context<HonoEnv>): Promise<Response> {
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
  const res = await resolveRequestModel(c, userId, modelRaw)
  if (res.kind !== "ok") {
    c.executionCtx.waitUntil(
      logRequest(c.env, {
        userId,
        apiKeyId,
        provider: loggingProviderFromRawModel(modelRaw),
        model: modelRaw.slice(0, 200),
        statusCode: res.kind === "group_not_found" ? 404 : 400,
        latencyMs: Date.now() - started,
        errorCode: res.kind === "group_not_found" ? "group_not_found" : "invalid_model",
      }),
    )
    if (res.kind === "group_not_found") {
      return c.json(
        { error: { message: `unknown group endpoint "${res.slug}"`, code: "not_found" } },
        404,
        { "x-should-retry": "false" },
      )
    }
    return c.json(
      {
        error: {
          message: res.groupSlug
            ? `model must be one of this group endpoint's configured models (see GET /g/${res.groupSlug}/openai/v1/models)`
            : "model must be provider/model (e.g. claude-code/claude-opus-5)",
          code: "invalid_model",
        },
      },
      400,
      { "x-should-retry": "false" },
    )
  }
  const resolved = res.resolution
  const effort = parseReasoningEffort(body.reasoning_effort)
  if (effort === "invalid") {
    return c.json(
      { error: { message: "invalid reasoning_effort", code: "invalid_reasoning" } },
      400,
    )
  }

  // Loop guard applies on conversion ingress (grok / codex / antigravity /
  // custom-openai); never on native Anthropic passthrough adapters
  // (claude-code / custom-anthropic). grok and antigravity expose `messages()`
  // for their /anthropic conversion paths, but /openai/v1 still uses
  // chatCompletions — keep them guarded (docs/api.md "Degenerate tool-call
  // loop guard"). Decided from the highest-priority resolved target only — a
  // structural, not usability-based, property (routing/candidates.ts `primary`).
  const nativeAnthropicPassthrough = isNativeAnthropicPassthrough(
    resolved.primary.provider,
    !!resolved.primary.adapter.messages,
  )
  if (!nativeAnthropicPassthrough) {
    const loop = detectOpenAIToolLoop((body.messages as unknown[]) ?? [])
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
          error: {
            message: loopDetectedMessage(loop),
            type: "invalid_request_error",
            code: "loop_detected",
          },
        },
        400,
        { "x-should-retry": "false" },
      )
    }
  }

  // Audio input (docs/api.md § Audio input). Decided from the highest-priority
  // resolved target only, same as the loop guard above: an adapter whose
  // upstream wire has no audio content part must fail the request rather than
  // drop the part and answer as if the client had sent silence.
  const audio = scanAudioParts((body.messages as unknown[]) ?? [])
  if (audio.present) {
    const audioInput = resolved.primary.adapter.audioInput
    const rejection = !audioInput
      ? {
          code: "unsupported_modality",
          message: `audio input is not supported by "${resolved.primary.provider}" — its upstream message format has no audio content part`,
        }
      : audioInput === "convert" && !audio.convertible
        ? {
            code: "unsupported_audio_format",
            message: `input_audio part is not convertible: needs base64 input_audio.data plus a format of ${SUPPORTED_AUDIO_FORMATS}, or a data: URL carrying its own mime`,
          }
        : null
    if (rejection) {
      c.executionCtx.waitUntil(
        logRequest(c.env, {
          userId,
          apiKeyId,
          provider: resolved.primary.provider,
          model: `${resolved.primary.provider}/${resolved.primary.upstreamModel}`,
          statusCode: 400,
          latencyMs: Date.now() - started,
          errorCode: rejection.code,
          groupName: resolved.groupName ?? null,
        }),
      )
      return c.json(
        {
          error: {
            message: rejection.message,
            type: "invalid_request_error",
            code: rejection.code,
          },
        },
        400,
        { "x-should-retry": "false" },
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
}

openaiRoutes.post("/chat/completions", handleChatCompletions)
openaiRoutes.post("/responses", handleResponses)

export async function handleAudioTranscriptions(c: Context<HonoEnv>): Promise<Response> {
  const started = Date.now()
  const userId = c.get("apiKeyUserId")!
  const apiKeyId = c.get("apiKeyId")

  let formData: FormData
  try {
    formData = await c.req.formData()
  } catch {
    return c.json(
      { error: { message: "Invalid multipart/form-data request", code: "invalid_request" } },
      400,
    )
  }

  const modelRaw = String(formData.get("model") ?? "").trim()
  if (!modelRaw) {
    c.executionCtx.waitUntil(
      logRequest(c.env, {
        userId,
        apiKeyId,
        provider: "unknown",
        model: "",
        statusCode: 400,
        latencyMs: Date.now() - started,
        errorCode: "invalid_model",
      }),
    )
    return c.json(
      {
        error: {
          message: "Missing required parameter: 'model'",
          type: "invalid_request_error",
          code: "invalid_model",
        },
      },
      400,
      { "x-should-retry": "false" },
    )
  }

  const file = formData.get("file")
  if (!file) {
    c.executionCtx.waitUntil(
      logRequest(c.env, {
        userId,
        apiKeyId,
        provider: loggingProviderFromRawModel(modelRaw),
        model: modelRaw.slice(0, 200),
        statusCode: 400,
        latencyMs: Date.now() - started,
        errorCode: "invalid_request",
      }),
    )
    return c.json(
      {
        error: {
          message: "Missing required parameter: 'file'",
          type: "invalid_request_error",
          code: "invalid_request",
        },
      },
      400,
      { "x-should-retry": "false" },
    )
  }

  const res = await resolveRequestModel(c, userId, modelRaw)
  if (res.kind !== "ok") {
    c.executionCtx.waitUntil(
      logRequest(c.env, {
        userId,
        apiKeyId,
        provider: loggingProviderFromRawModel(modelRaw),
        model: modelRaw.slice(0, 200),
        statusCode: res.kind === "group_not_found" ? 404 : 400,
        latencyMs: Date.now() - started,
        errorCode: res.kind === "group_not_found" ? "group_not_found" : "invalid_model",
      }),
    )
    if (res.kind === "group_not_found") {
      return c.json(
        { error: { message: `unknown group endpoint "${res.slug}"`, code: "not_found" } },
        404,
        { "x-should-retry": "false" },
      )
    }
    return c.json(
      {
        error: {
          message: res.groupSlug
            ? `model must be one of this group endpoint's configured models (see GET /g/${res.groupSlug}/openai/v1/models)`
            : "model must be provider/model (e.g. openrouter/openai/whisper-large-v3-turbo)",
          code: "invalid_model",
        },
      },
      400,
      { "x-should-retry": "false" },
    )
  }

  const resolved = res.resolution
  if (!resolved.primary.adapter.audioTranscriptions) {
    c.executionCtx.waitUntil(
      logRequest(c.env, {
        userId,
        apiKeyId,
        provider: resolved.primary.provider,
        model: `${resolved.primary.provider}/${resolved.primary.upstreamModel}`,
        statusCode: 400,
        latencyMs: Date.now() - started,
        errorCode: "unsupported_modality",
        groupName: resolved.groupName ?? null,
      }),
    )
    return c.json(
      {
        error: {
          message: `audio transcription is not supported by "${resolved.primary.provider}" — only custom OpenAI-format providers support the audio/transcriptions endpoint`,
          type: "invalid_request_error",
          code: "unsupported_modality",
        },
      },
      400,
      { "x-should-retry": "false" },
    )
  }

  return dispatchAudioTranscriptions(c.env, {
    userId,
    apiKeyId,
    provider: resolved.primary.provider,
    adapter: resolved.primary.adapter,
    formData,
    rawModel: modelRaw,
    upstreamModel: resolved.primary.upstreamModel,
    waitUntil: (p) => c.executionCtx.waitUntil(p),
    groupName: resolved.groupName,
    candidates: resolved.candidates,
    strategy: resolved.strategy,
    isBuiltin: resolved.primary.isBuiltin,
    customProvider: resolved.primary.customProvider,
  })
}

openaiRoutes.post("/audio/transcriptions", handleAudioTranscriptions)
