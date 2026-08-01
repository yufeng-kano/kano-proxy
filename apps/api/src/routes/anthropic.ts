import { Hono } from "hono"
import { apiKeyAuth } from "../auth/api_key_auth"
import type { HonoEnv } from "../auth/session"
import { listModelsForUser } from "../catalog/models"
import { dispatchAnthropicMessages } from "../proxy/dispatch"
import { parseAnthropicModel } from "../utils/model"

export const anthropicRoutes = new Hono<HonoEnv>()

anthropicRoutes.use("*", apiKeyAuth)

anthropicRoutes.get("/v1/models", async (c) => {
  const userId = c.get("apiKeyUserId")!
  const { models } = await listModelsForUser(c.env, userId, { availableOnly: true })
  // Anthropic surface: only claude-code models, bare upstream ids
  const claude = models.filter((m) => m.provider === "claude-code")
  return c.json({
    data: claude.map((m) => ({
      id: m.upstream,
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
  if (!parsed || parsed.provider !== "claude-code") {
    return c.json(
      {
        type: "error",
        error: {
          type: "invalid_request_error",
          message: "Anthropic surface only supports claude-code models",
        },
      },
      400,
    )
  }
  // Force upstream model id (strip provider prefix if present)
  body.model = parsed.upstreamModel
  // Strict passthrough of cache_control — we do not touch body fields beyond model normalize + required system in adapter

  const headers = new Headers()
  const beta = c.req.header("anthropic-beta")
  const ver = c.req.header("anthropic-version")
  if (beta) headers.set("anthropic-beta", beta)
  if (ver) headers.set("anthropic-version", ver)

  return dispatchAnthropicMessages(c.env, {
    userId,
    apiKeyId,
    body,
    headers,
    model: parsed.raw,
  })
})
