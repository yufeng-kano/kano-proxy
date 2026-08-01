import type { MiddlewareHandler } from "hono"
import { extractBearer, hashApiKey } from "../crypto/keys"
import { findKeyByHash, touchKey } from "../db/keys"
import type { HonoEnv } from "./session"

/** Resolve sk-kano-proxy API key from Authorization or x-api-key. */
export const apiKeyAuth: MiddlewareHandler<HonoEnv> = async (c, next) => {
  // Error envelope is surface-shaped, not provider-shaped: Anthropic clients
  // on /anthropic expect the Anthropic error shape even for auth failures.
  const isAnthropic = c.req.path.startsWith("/anthropic")
  const unauthorized = (message: string) =>
    isAnthropic
      ? c.json({ type: "error", error: { type: "authentication_error", message } }, 401)
      : c.json(
          { error: { message, type: "invalid_request_error", code: "invalid_api_key" } },
          401,
        )

  const bearer = extractBearer(c.req.header("authorization"))
  const xKey = c.req.header("x-api-key")
  const raw = bearer ?? xKey ?? null
  if (!raw) return unauthorized("Missing API key")
  const hash = await hashApiKey(raw)
  const row = await findKeyByHash(c.env.DB, hash)
  if (!row) return unauthorized("Invalid API key")
  c.set("apiKeyUserId", row.user_id)
  c.set("apiKeyId", row.id)
  c.executionCtx.waitUntil(touchKey(c.env.DB, row.id))
  await next()
}
