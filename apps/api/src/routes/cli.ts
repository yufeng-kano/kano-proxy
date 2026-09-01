/**
 * /api/cli — session-authenticated management for the web UI's CLI page and
 * its authorize view (docs/cli.md § Web UI, docs/auth.md § CLI devices and
 * providers). Same origin-locked CORS as every /api/* route.
 */

import { Hono } from "hono"
import { newPairingCode, sha256Hex } from "../auth/cli_tokens"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import {
  approveLoginRequest,
  deleteLoginRequest,
  getCliProviderById,
  getLoginRequest,
  listCliDevices,
  renameCliProvider,
  revokeCliDevice,
} from "../db/cli"
import { validateName } from "../utils/custom_provider"
import { listCliProviderItems, removeCliProvider } from "./cli_shared"

export const cliRoutes = new Hono<HonoEnv>()

async function requireUser(c: {
  env: HonoEnv["Bindings"]
  req: { header: (n: string) => string | undefined }
}) {
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  return loaded?.user ?? null
}

cliRoutes.get("/devices", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const rows = await listCliDevices(c.env.DB, user.id)
  return c.json({
    devices: rows.map((row) => ({
      id: row.id,
      name: row.name,
      last_seen_at: row.last_seen_at,
      created_at: row.created_at,
      revoked_at: row.revoked_at,
    })),
  })
})

cliRoutes.post("/devices/:id/revoke", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  // Idempotent: revoking an already-revoked device is still ok. The 404 is
  // only for a device that is not the caller's at all.
  const rows = await listCliDevices(c.env.DB, user.id)
  if (!rows.some((row) => row.id === c.req.param("id"))) return c.json({ error: "not found" }, 404)
  await revokeCliDevice(c.env.DB, user.id, c.req.param("id"))
  return c.json({ ok: true })
})

cliRoutes.get("/providers", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  return c.json({ providers: await listCliProviderItems(c.env, user.id) })
})

cliRoutes.patch("/providers/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const row = await getCliProviderById(c.env.DB, user.id, c.req.param("id"))
  if (!row) return c.json({ error: "not found" }, 404)
  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid JSON" }, 400)
  }
  const name = typeof body.name === "string" ? body.name.trim() : ""
  const nameErr = validateName(name)
  if (nameErr) return c.json({ error: nameErr }, 400)
  await renameCliProvider(c.env.DB, user.id, row.id, name)
  return c.json({ ok: true, name })
})

cliRoutes.delete("/providers/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const row = await getCliProviderById(c.env.DB, user.id, c.req.param("id"))
  if (!row) return c.json({ error: "not found" }, 404)
  await removeCliProvider(c.env, user.id, row)
  return c.json({ ok: true })
})

cliRoutes.get("/login-requests/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const row = await getLoginRequest(c.env.DB, c.req.param("id"))
  if (!row || new Date(row.expires_at).getTime() < Date.now()) {
    return c.json({ error: "not found" }, 404)
  }
  return c.json({
    id: row.id,
    device_name: row.device_name,
    expires_at: row.expires_at,
    approved: !!row.approved_at,
    used: !!row.used_at,
  })
})

cliRoutes.post("/login-requests/:id/approve", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const row = await getLoginRequest(c.env.DB, c.req.param("id"))
  if (!row || new Date(row.expires_at).getTime() < Date.now()) {
    return c.json({ error: "not found" }, 404)
  }
  if (row.used_at || row.approved_at) {
    return c.json({ error: "already approved" }, 400)
  }
  // The plaintext code exists exactly here and in this response — the row
  // stores only its hash, and a page refresh can never re-show it.
  const code = newPairingCode()
  const approved = await approveLoginRequest(c.env.DB, row.id, user.id, await sha256Hex(code.replace("-", "")))
  if (!approved) return c.json({ error: "already approved" }, 400)
  return c.json({ ok: true, code })
})

cliRoutes.post("/login-requests/:id/deny", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const row = await getLoginRequest(c.env.DB, c.req.param("id"))
  if (!row) return c.json({ error: "not found" }, 404)
  await deleteLoginRequest(c.env.DB, row.id)
  return c.json({ ok: true })
})
