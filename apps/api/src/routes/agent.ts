/**
 * /agent/v1 — the kano-proxy CLI's own namespace (docs/cli.md § Server
 * routes): device login, refresh-token rotation, provider CRUD, and the
 * WebSocket connect that forwards into the AgentTunnel DO. Token-
 * authenticated (never session), no CORS (no browser callers).
 */

import { Hono } from "hono"
import type { Context } from "hono"
import {
  mintAccessToken,
  newRefreshToken,
  normalizePairingCode,
  sha256Hex,
  verifyAccessToken,
  type CliTokenClaims,
} from "../auth/cli_tokens"
import type { HonoEnv } from "../auth/session"
import { timingSafeEqual } from "../auth/session"
import { insertAccount } from "../db/accounts"
import {
  MAX_CLI_DEVICES_PER_USER,
  MAX_LOGIN_CODE_ATTEMPTS,
  countCliDevices,
  countCliProviders,
  deleteCliProvider,
  findCliDeviceByPrevRefreshHash,
  findCliDeviceByRefreshHash,
  getCliDevice,
  getCliProviderById,
  getCliProviderBySlug,
  getLoginRequest,
  insertCliDevice,
  insertCliProvider,
  insertLoginRequest,
  markLoginRequestUsed,
  recordLoginCodeAttempt,
  rotateCliDeviceRefreshToken,
  touchCliDeviceLastSeen,
  type CliDeviceRow,
} from "../db/cli"
import { getCustomProviderBySlug, countCustomProviders } from "../db/custom_providers"
import { encryptJson } from "../crypto/token_crypto"
import {
  INTERNAL_EXP_HEADER,
  INTERNAL_FORMAT_HEADER,
  INTERNAL_PROVIDER_HEADER,
  INTERNAL_SLUG_HEADER,
  INTERNAL_USER_HEADER,
} from "../do/agent_tunnel"
import { validateModelsReport } from "../do/protocol"
import {
  MAX_CUSTOM_PROVIDERS_PER_USER,
  isCustomProviderFormat,
  validateName,
  validateSlug,
} from "../utils/custom_provider"
import { agentTunnelStub, listCliProviderItems, removeCliProvider } from "./cli_shared"

export const agentRoutes = new Hono<HonoEnv>()

agentRoutes.post("/login/start", async (c) => {
  // Fail closed at the *first* request when device auth is unprovisioned —
  // otherwise the user walks the whole browser approval before
  // /login/complete can only 500, and the disabled endpoint still
  // accumulates request rows.
  if (!c.env.CLI_TOKEN_SECRET) return c.json({ error: "CLI_TOKEN_SECRET not configured" }, 500)
  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid JSON" }, 400)
  }
  const deviceName = typeof body.device_name === "string" ? body.device_name.trim() : ""
  const nameErr = validateName(deviceName)
  if (nameErr) return c.json({ error: `device_name: ${nameErr}` }, 400)
  // Per-IP budget enforced atomically inside the INSERT (docs/cli.md
  // § Security notes) — a KV read-modify-write here was bypassable by
  // parallel batches from one address. Only the IP's hash is stored.
  const ipHash = await sha256Hex(c.req.header("cf-connecting-ip") || "unknown")
  const row = await insertLoginRequest(c.env.DB, deviceName, ipHash)
  if (!row) {
    return c.json({ error: "too many login attempts — try again later" }, 429)
  }
  const appUrl = (c.env.APP_URL || "").replace(/\/$/, "")
  return c.json({
    request_id: row.id,
    verify_url: `${appUrl}/cli/authorize?request=${row.id}`,
    expires_at: row.expires_at,
  })
})

agentRoutes.post("/login/complete", async (c) => {
  if (!c.env.CLI_TOKEN_SECRET) return c.json({ error: "CLI_TOKEN_SECRET not configured" }, 500)
  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid JSON" }, 400)
  }
  const requestId = typeof body.request_id === "string" ? body.request_id : ""
  const code = typeof body.code === "string" ? normalizePairingCode(body.code) : ""
  if (!requestId || !code) return c.json({ error: "request_id and code are required" }, 400)

  const row = await getLoginRequest(c.env.DB, requestId)
  if (!row || row.used_at || new Date(row.expires_at).getTime() < Date.now()) {
    return c.json({ error: "login request expired or already used — run init again" }, 401)
  }
  if (!row.approved_at || !row.user_id || !row.code_hash) {
    return c.json({ error: "login request not approved yet" }, 401)
  }
  if (row.attempts >= MAX_LOGIN_CODE_ATTEMPTS) {
    return c.json({ error: "too many wrong codes — run init again" }, 401)
  }
  const presented = await sha256Hex(code)
  if (!timingSafeEqual(presented, row.code_hash)) {
    await recordLoginCodeAttempt(c.env.DB, requestId)
    return c.json({ error: "wrong code" }, 401)
  }
  // Single-use, atomically: two racing completes cannot both mint a device.
  if (!(await markLoginRequestUsed(c.env.DB, requestId))) {
    return c.json({ error: "login request expired or already used — run init again" }, 401)
  }
  if ((await countCliDevices(c.env.DB, row.user_id)) >= MAX_CLI_DEVICES_PER_USER) {
    return c.json({ error: `maximum of ${MAX_CLI_DEVICES_PER_USER} devices reached` }, 400)
  }

  const refreshToken = newRefreshToken()
  const device = await insertCliDevice(c.env.DB, {
    userId: row.user_id,
    name: row.device_name,
    refreshTokenHash: await sha256Hex(refreshToken),
  })
  const access = await mintAccessToken(c.env.CLI_TOKEN_SECRET, { userId: row.user_id, deviceId: device.id })
  return c.json({
    device_id: device.id,
    refresh_token: refreshToken,
    access_token: access.token,
    expires_in: access.expiresIn,
  })
})

agentRoutes.post("/token", async (c) => {
  if (!c.env.CLI_TOKEN_SECRET) return c.json({ error: "CLI_TOKEN_SECRET not configured" }, 500)
  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid JSON" }, 400)
  }
  const presented = typeof body.refresh_token === "string" ? body.refresh_token : ""
  if (!presented) return c.json({ error: "refresh_token is required" }, 400)
  const presentedHash = await sha256Hex(presented)

  const device = await findCliDeviceByRefreshHash(c.env.DB, presentedHash)
  if (device) {
    if (device.revoked_at) return c.json({ error: "device_revoked" }, 401)
    const next = newRefreshToken()
    const rotated = await rotateCliDeviceRefreshToken(c.env.DB, device.id, presentedHash, await sha256Hex(next))
    // Lost the race to a concurrent presentation of the same token — that
    // sibling already rotated; this caller retries against its state file.
    if (!rotated) return c.json({ error: "invalid refresh token" }, 401)
    const access = await mintAccessToken(c.env.CLI_TOKEN_SECRET, { userId: device.user_id, deviceId: device.id })
    return c.json({ refresh_token: next, access_token: access.token, expires_in: access.expiresIn })
  }

  // A superseded token is treated as theft: revoke the whole device family
  // (docs/cli.md § Device auth). A token matching nothing is a plain 401.
  const stale = await findCliDeviceByPrevRefreshHash(c.env.DB, presentedHash)
  if (stale && !stale.revoked_at) {
    await c.env.DB.prepare(`UPDATE cli_devices SET revoked_at = ? WHERE id = ?`)
      .bind(new Date().toISOString(), stale.id)
      .run()
    console.log(`[agent] refresh-token reuse revoked device ${stale.id}`)
    return c.json({ error: "device_revoked" }, 401)
  }
  return c.json({ error: "invalid refresh token" }, 401)
})

/** Bearer access token → claims + non-revoked device row, or null (caller answers 401). */
async function authenticateDevice(
  c: Context<HonoEnv>,
): Promise<{ claims: CliTokenClaims; device: CliDeviceRow } | null> {
  const secret = c.env.CLI_TOKEN_SECRET
  if (!secret) return null
  const header = c.req.header("authorization") || ""
  const match = header.match(/^Bearer\s+(.+)$/i)
  if (!match) return null
  const claims = await verifyAccessToken(secret, match[1]!.trim())
  if (!claims) return null
  const device = await getCliDevice(c.env.DB, claims.device_id)
  if (!device || device.user_id !== claims.user_id || device.revoked_at) return null
  return { claims, device }
}

agentRoutes.get("/providers", async (c) => {
  const auth = await authenticateDevice(c)
  if (!auth) return c.json({ error: "unauthorized" }, 401)
  return c.json({ providers: await listCliProviderItems(c.env, auth.claims.user_id) })
})

agentRoutes.post("/providers", async (c) => {
  const auth = await authenticateDevice(c)
  if (!auth) return c.json({ error: "unauthorized" }, 401)
  if (!c.env.TOKEN_ENCRYPTION_KEY) return c.json({ error: "TOKEN_ENCRYPTION_KEY not configured" }, 500)
  const userId = auth.claims.user_id

  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid JSON" }, 400)
  }

  const slug = typeof body.slug === "string" ? body.slug.trim().toLowerCase() : ""
  const slugErr = validateSlug(slug)
  if (slugErr) return c.json({ error: slugErr }, 400)
  const format = body.format
  if (!isCustomProviderFormat(format)) {
    return c.json({ error: "format must be 'openai' or 'anthropic'" }, 400)
  }
  const name = typeof body.name === "string" && body.name.trim() ? body.name.trim() : slug
  const nameErr = validateName(name)
  if (nameErr) return c.json({ error: nameErr }, 400)

  // Expose whitelist and probe-failure manual seed share the report bounds
  // (docs/cli.md § Model catalog); the agent's first connect overwrites the seed.
  let modelFilterJson: string | null = null
  if (body.expose !== undefined) {
    if (!Array.isArray(body.expose) || body.expose.some((m) => typeof m !== "string")) {
      return c.json({ error: "expose must be an array of model id strings" }, 400)
    }
    const validated = validateModelsReport(body.expose as string[])
    if (validated === null) return c.json({ error: "expose entries are out of bounds" }, 400)
    modelFilterJson = validated.length ? JSON.stringify(validated) : null
  }
  let modelsJson: string | null = null
  if (body.initial_models !== undefined) {
    if (!Array.isArray(body.initial_models) || body.initial_models.some((m) => typeof m !== "string")) {
      return c.json({ error: "initial_models must be an array of model id strings" }, 400)
    }
    const validated = validateModelsReport(body.initial_models as string[])
    if (validated === null) return c.json({ error: "initial_models entries are out of bounds" }, 400)
    modelsJson = validated.length ? JSON.stringify(validated) : null
  }

  // Shared slug namespace and shared 20-per-user cap with custom providers.
  const total = (await countCustomProviders(c.env.DB, userId)) + (await countCliProviders(c.env.DB, userId))
  if (total >= MAX_CUSTOM_PROVIDERS_PER_USER) {
    return c.json({ error: `maximum of ${MAX_CUSTOM_PROVIDERS_PER_USER} providers reached (custom + CLI)` }, 400)
  }
  if (await getCliProviderBySlug(c.env.DB, userId, slug)) {
    return c.json({ error: `slug "${slug}" is already in use` }, 409)
  }
  if (await getCustomProviderBySlug(c.env.DB, userId, slug)) {
    return c.json({ error: `slug "${slug}" is already in use by a custom provider` }, 409)
  }

  const row = await insertCliProvider(c.env.DB, {
    userId,
    deviceId: auth.device.id,
    slug,
    name,
    format,
    modelsJson,
    modelFilterJson,
  })
  // Lost an insert race against a concurrent custom create for this slug.
  if (!row) return c.json({ error: `slug "${slug}" is already in use by a custom provider` }, 409)
  // The internal pool-state row (docs/cli.md § Data model): a placeholder
  // credential that decrypts fine and authorizes nothing — the CLI injects
  // the local server's real key on its side of the tunnel. D1 has no
  // cross-statement transaction here, so a failed account write compensates
  // by deleting the provider row — otherwise a slug-reserving orphan answers
  // no_upstream_account until manually deleted.
  try {
    const encrypted = await encryptJson(c.env.TOKEN_ENCRYPTION_KEY, { access_token: "" })
    await insertAccount(c.env.DB, {
      userId,
      provider: slug,
      encryptedPayload: encrypted,
      label: name,
    })
  } catch (error) {
    try {
      await deleteCliProvider(c.env.DB, userId, row.id)
    } catch {
      /* best-effort — the web UI's delete remains the fallback */
    }
    console.error("[agent] provider account insert failed", {
      providerId: row.id,
      error: error instanceof Error ? error.message : String(error),
    })
    return c.json({ error: "could not create the provider — try again" }, 500)
  }

  return c.json(
    {
      id: row.id,
      slug: row.slug,
      name: row.name,
      format: row.format,
      created_at: row.created_at,
    },
    201,
  )
})

agentRoutes.delete("/providers/:id", async (c) => {
  const auth = await authenticateDevice(c)
  if (!auth) return c.json({ error: "unauthorized" }, 401)
  const row = await getCliProviderById(c.env.DB, auth.claims.user_id, c.req.param("id"))
  if (!row) return c.json({ error: "not found" }, 404)
  await removeCliProvider(c.env, auth.claims.user_id, row)
  return c.json({ ok: true })
})

agentRoutes.get("/connect/:providerId", async (c) => {
  if (c.req.header("upgrade")?.toLowerCase() !== "websocket") {
    return c.json({ error: "expected websocket upgrade" }, 426)
  }
  const auth = await authenticateDevice(c)
  if (!auth) return c.json({ error: "unauthorized" }, 401)
  const row = await getCliProviderById(c.env.DB, auth.claims.user_id, c.req.param("providerId"))
  if (!row) return c.json({ error: "not found" }, 404)
  const stub = agentTunnelStub(c.env, row.id)
  if (!stub) return c.json({ error: "agent tunnel not configured" }, 500)

  await touchCliDeviceLastSeen(c.env.DB, auth.device.id)

  // The DO never sees or validates tokens — it trusts these internal headers
  // set here, after the Worker has verified everything (docs/cli.md § Wire
  // protocol). The token's exp drives the DO's revocation alarm.
  const headers = new Headers(c.req.raw.headers)
  headers.delete("authorization")
  headers.set(INTERNAL_USER_HEADER, auth.claims.user_id)
  headers.set(INTERNAL_PROVIDER_HEADER, row.id)
  headers.set(INTERNAL_SLUG_HEADER, row.slug)
  headers.set(INTERNAL_FORMAT_HEADER, row.format)
  headers.set(INTERNAL_EXP_HEADER, String(auth.claims.exp * 1000))
  return stub.fetch("https://agent-tunnel/connect", { headers })
})
