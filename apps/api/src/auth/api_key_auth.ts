import type { MiddlewareHandler } from "hono"
import { extractBearer, hashApiKey } from "../crypto/keys"
import { findKeyByHash, touchKey } from "../db/keys"
import { logRequest } from "../logging/request_log"
import { keyWindowSpendCached } from "./spend_limit"
import type { HonoEnv } from "./session"

/** Resolve sk-kano-proxy API key from Authorization or x-api-key. */
export const apiKeyAuth: MiddlewareHandler<HonoEnv> = async (c, next) => {
  // Error envelope is surface-shaped, not provider-shaped: Anthropic clients
  // on /anthropic (shared base or a group endpoint's anthropic mount) expect
  // the Anthropic error shape even for auth failures.
  const isAnthropic =
    c.req.path.startsWith("/anthropic") || /^\/g\/[^/]+\/anthropic(\/|$)/.test(c.req.path)
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

  // Spend-limit gate (docs/pricing.md): POST surfaces only — GET /models
  // stays free so a blocked client can still see its catalog. A null window
  // sum is a D1 failure and fails open; a real at-or-over read fails closed.
  if (row.spend_limit != null && c.req.method === "POST") {
    const spend = await keyWindowSpendCached(c.env, row)
    if (spend != null && spend >= row.spend_limit) {
      const message = `Spend limit reached for this API key ($${row.spend_limit} per ${row.spend_limit_interval})`
      c.executionCtx.waitUntil(
        logRequest(c.env, {
          userId: row.user_id,
          apiKeyId: row.id,
          provider: "unknown",
          model: "",
          statusCode: 429,
          latencyMs: 0,
          errorCode: "spend_limit_exceeded",
        }),
      )
      return isAnthropic
        ? c.json({ type: "error", error: { type: "rate_limit_error", message } }, 429, { "x-should-retry": "false" })
        : c.json(
            { error: { message, type: "rate_limit_error", code: "spend_limit_exceeded" } },
            429,
            { "x-should-retry": "false" },
          )
    }
  }

  c.set("apiKeyUserId", row.user_id)
  c.set("apiKeyId", row.id)
  c.executionCtx.waitUntil(touchKey(c.env.DB, row.id))
  await next()
}
