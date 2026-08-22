import { Hono } from "hono"
import type { Context } from "hono"
import { apiKeyAuth } from "../auth/api_key_auth"
import type { HonoEnv } from "../auth/session"
import { getGroupBySlug, listModelsForGroup, type ModelGroupRow } from "../db/model_groups"
import { handleAnthropicCountTokens, handleAnthropicMessages } from "./anthropic"
import { handleChatCompletions } from "./openai"

/**
 * Model-group virtual endpoints (docs/api.md § Group endpoints): each group
 * is served at `/g/<slug>/openai/v1/*` and `/g/<slug>/anthropic/*`, mirroring
 * the shared bases. The POST handlers are the shared-surface ones — they
 * branch on the `slug` param inside `resolveRequestModel` — so only the
 * per-group `/models` catalogs live here.
 */
export const groupEndpointRoutes = new Hono<HonoEnv>()

groupEndpointRoutes.use("*", apiKeyAuth)

/** The caller's group behind the path slug, or `null` (the route's 404). */
async function groupForRequest(c: Context<HonoEnv>): Promise<ModelGroupRow | null> {
  const userId = c.get("apiKeyUserId")!
  return getGroupBySlug(c.env.DB, userId, c.req.param("slug") ?? "")
}

/**
 * A group's own catalog: exactly its model names, listed regardless of
 * current target usability (docs/providers.md § Model groups "Catalog").
 */
groupEndpointRoutes.get("/:slug/openai/v1/models", async (c) => {
  const group = await groupForRequest(c)
  if (!group) {
    return c.json(
      { error: { message: `unknown group endpoint "${c.req.param("slug")}"`, code: "not_found" } },
      404,
    )
  }
  const models = await listModelsForGroup(c.env.DB, group.id)
  return c.json({
    object: "list",
    data: models.map((m) => ({
      id: m.name,
      object: "model",
      owned_by: "group",
      display_name: m.name,
    })),
  })
})

groupEndpointRoutes.get("/:slug/anthropic/v1/models", async (c) => {
  const group = await groupForRequest(c)
  if (!group) {
    return c.json(
      {
        type: "error",
        error: { type: "not_found_error", message: `unknown group endpoint "${c.req.param("slug")}"` },
      },
      404,
    )
  }
  const models = await listModelsForGroup(c.env.DB, group.id)
  return c.json({
    data: models.map((m) => ({
      id: m.name,
      display_name: m.name,
      type: "model",
    })),
  })
})

groupEndpointRoutes.post("/:slug/openai/v1/chat/completions", handleChatCompletions)
groupEndpointRoutes.post("/:slug/anthropic/v1/messages", handleAnthropicMessages)
groupEndpointRoutes.post("/:slug/anthropic/v1/messages/count_tokens", handleAnthropicCountTokens)
