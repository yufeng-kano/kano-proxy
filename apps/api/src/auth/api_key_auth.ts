import type { MiddlewareHandler } from "hono"
import { extractBearer, hashApiKey } from "../crypto/keys"
import { findKeyByHash, touchKey } from "../db/keys"
import type { HonoEnv } from "./session"

/** Resolve sk-kano-proxy API key from Authorization or x-api-key. */
export const apiKeyAuth: MiddlewareHandler<HonoEnv> = async (c, next) => {
  const bearer = extractBearer(c.req.header("authorization"))
  const xKey = c.req.header("x-api-key")
  const raw = bearer ?? xKey ?? null
  if (!raw) {
    return c.json(
      { error: { message: "Missing API key", type: "invalid_request_error", code: "invalid_api_key" } },
      401,
    )
  }
  const hash = await hashApiKey(raw)
  const row = await findKeyByHash(c.env.DB, hash)
  if (!row) {
    return c.json(
      { error: { message: "Invalid API key", type: "invalid_request_error", code: "invalid_api_key" } },
      401,
    )
  }
  c.set("apiKeyUserId", row.user_id)
  c.set("apiKeyId", row.id)
  c.executionCtx.waitUntil(touchKey(c.env.DB, row.id))
  await next()
}
