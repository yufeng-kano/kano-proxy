import { Hono } from "hono"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import {
  beginClaudeAuthorization,
  CODEX_DEVICE_AUTH,
  exchangeClaudeCode,
  extractChatgptAccountId,
  extractJwtExpiryIso,
  type PendingOAuth,
} from "../auth/provider_oauth"
import { parseCodeHashState } from "../auth/pkce"
import {
  acquireUsageLock,
  getAccount,
  insertAccount,
  isUsageFresh,
  listAccounts,
  promoteAccount,
  readUsageSnapshot,
  removeAccount,
  setAccountCustomLabel,
} from "../db/accounts"
import { getProviderStrategy, setProviderStrategy } from "../db/provider_settings"
import { DEFAULT_STRATEGY } from "../routing/strategy"
import {
  fetchClaudeIdentity,
  fetchCodexIdentity,
  fetchGrokIdentity,
  pickAccountLabel,
} from "../providers/identity"
import { fetchAndPersistUsage } from "../providers/usage_refresh"
import { encryptJson } from "../crypto/token_crypto"
import { isProviderId, type ProviderId } from "../env"
import { clearBench, isBenched } from "../pool/bench"
import type { StoredCredential } from "../pool/acquire"
import { getAdapter } from "../providers"
import { newId, nowIso } from "../utils/id"

export const providerRoutes = new Hono<HonoEnv>()

async function requireUser(c: {
  env: HonoEnv["Bindings"]
  req: { header: (n: string) => string | undefined }
}) {
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  return loaded?.user ?? null
}

function parseProvider(p: string): ProviderId | null {
  return isProviderId(p) ? p : null
}

providerRoutes.get("/:provider/accounts", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const provider = parseProvider(c.req.param("provider"))
  if (!provider) return c.json({ error: "invalid provider" }, 400)

  // Usage is cached 60s server-side in D1 behind a single-flight lock, so N
  // devices cost one upstream call, not N (docs/providers.md § Usage cache).
  const force = c.req.query("refresh") === "true"
  const rows = await listAccounts(c.env.DB, user.id, provider)
  const adapter = getAdapter(provider)
  const accounts = []
  let priority = 0
  for (const row of rows) {
    const benched = await isBenched(c.env, user.id, provider, row.id)
    let status: "active" | "standby" | "benched" | "unusable" = benched
      ? "benched"
      : priority === 0
        ? "active"
        : "standby"
    if (!benched) priority++

    let usage = null
    let accountMeta: Record<string, unknown> | null = row.account_meta_json
      ? (JSON.parse(row.account_meta_json) as Record<string, unknown>)
      : null
    let error: string | null = null
    let stale = false

    if (adapter.fetchUsage) {
      // Fresh cache is the only path that skips upstream. Everything else —
      // stale, missing, or an explicit user Refresh — fetches synchronously and
      // returns the fresh result: revalidating in the background would render
      // one poll cycle behind forever, since the 90s frontend poll always
      // arrives after the 60s TTL has expired.
      const fresh = !force && isUsageFresh(row)
      // A malformed blob reads as a miss, never as trusted data, so an
      // unparseable snapshot inside the TTL falls through to a real fetch.
      let cached = fresh ? readUsageSnapshot(row) : null
      const lockToken = cached ? null : await acquireUsageLock(c.env.DB, row.id)
      // Lost the lock: another request is already calling upstream, so serve
      // whatever snapshot exists rather than queueing behind it.
      if (!cached && !lockToken) cached = readUsageSnapshot(row)

      if (cached) {
        usage = { windows: cached.windows }
        accountMeta = { ...accountMeta, ...cached.account }
        // Anything served past its TTL is flagged stale, even though the
        // in-flight fetch that beat us here will refresh it shortly.
        stale = cached.stale || !fresh
        error = cached.error
        // Re-derived from the snapshot, not recomputed from a live call: the
        // stored error/edgeBlocked exist precisely so a cache hit cannot flip
        // an unusable account back to active.
        if (
          status !== "benched" &&
          error &&
          !cached.windows.length &&
          !cached.edgeBlocked &&
          /401|invalid.?token|unauthorized/i.test(error)
        ) {
          status = "unusable"
        }
      } else if (lockToken) {
        // Shared core with the routing module's background refresh
        // (docs/providers.md § Usage cache / § Routing module "Facts") —
        // the lock/fetch/write sequence itself lives in `usage_refresh.ts`,
        // never duplicated here.
        const result = await fetchAndPersistUsage(c.env, row, adapter, lockToken)
        if (result.ok) {
          const snap = result.snapshot
          usage = { windows: snap.windows }
          accountMeta = { ...accountMeta, ...snap.account }
          stale = snap.stale
          error = snap.error
          // Usage 403 bot-wall must NOT mark the account unusable — chat can still work.
          if (
            status !== "benched" &&
            error &&
            !snap.windows.length &&
            !snap.edgeBlocked &&
            /401|invalid.?token|unauthorized/i.test(error)
          ) {
            status = "unusable"
          }
        } else {
          error = result.error
          stale = true
          // Serve the unchanged snapshot too: one upstream hiccup must not blank
          // the usage bars for the request that encountered it.
          const cachedAfterFailure = readUsageSnapshot(row)
          if (cachedAfterFailure) {
            usage = { windows: cachedAfterFailure.windows }
            accountMeta = { ...accountMeta, ...cachedAfterFailure.account }
            if (
              status !== "benched" &&
              !cachedAfterFailure.windows.length &&
              !cachedAfterFailure.edgeBlocked &&
              /401|invalid.?token|unauthorized/i.test(error)
            ) {
              status = "unusable"
            }
          }
        }
      }
    }

    const displayLabel =
      row.custom_label ||
      (typeof accountMeta?.email === "string" && accountMeta.email) ||
      (typeof accountMeta?.display_name === "string" && accountMeta.display_name) ||
      (typeof accountMeta?.username === "string" && accountMeta.username) ||
      row.label ||
      row.id

    accounts.push({
      id: row.id,
      priority: row.priority,
      status,
      label: displayLabel,
      custom_label: row.custom_label ?? null,
      account: accountMeta,
      usage,
      error,
      stale,
    })
  }

  // fix active: first non-benched
  let seen = false
  for (const a of accounts) {
    if (a.status === "benched" || a.status === "unusable") continue
    a.status = seen ? "standby" : "active"
    seen = true
  }

  // The pool's routing strategy (docs/providers.md § Routing module) — no
  // separate read route; `PATCH /api/providers/:provider` is the only
  // writer (docs/auth.md § Management routes).
  const strategy = await getProviderStrategy(c.env.DB, user.id, provider)

  return c.json({ available: true, accounts, models: [], error: null, strategy })
})

/**
 * Sets the pool's routing strategy (docs/providers.md § Routing module):
 * body `{strategy}`, only `ordered` accepted today. Upserts
 * `provider_settings` — a missing row means `ordered`, so this is the only
 * writer of that table.
 */
providerRoutes.patch("/:provider", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const provider = parseProvider(c.req.param("provider"))
  if (!provider) return c.json({ error: "invalid provider" }, 400)

  let body: unknown
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid strategy" }, 400)
  }
  const raw = body !== null && typeof body === "object" ? (body as { strategy?: unknown }).strategy : undefined
  if (typeof raw !== "string" || raw !== DEFAULT_STRATEGY) {
    return c.json({ error: `strategy must be "${DEFAULT_STRATEGY}"` }, 400)
  }

  await setProviderStrategy(c.env.DB, user.id, provider, raw)
  return c.json({ ok: true, strategy: raw })
})

providerRoutes.patch("/:provider/accounts/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)

  let body: unknown
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid custom_label" }, 400)
  }
  const rawLabel =
    body !== null && typeof body === "object" && "custom_label" in body
      ? (body as { custom_label?: unknown }).custom_label
      : undefined
  let customLabel: string | null
  if (rawLabel === null) {
    customLabel = null
  } else if (typeof rawLabel !== "string") {
    return c.json({ error: "invalid custom_label" }, 400)
  } else {
    customLabel = rawLabel.trim()
    if (!customLabel) customLabel = null
    if (customLabel && customLabel.length > 64) {
      return c.json({ error: "custom_label too long" }, 400)
    }
  }

  const ok = await setAccountCustomLabel(c.env.DB, user.id, c.req.param("id"), customLabel)
  if (!ok) return c.json({ error: "not found" }, 404)
  return c.json({ ok: true, custom_label: customLabel })
})

providerRoutes.post("/:provider/accounts/:id/promote", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const ok = await promoteAccount(c.env.DB, user.id, c.req.param("id"))
  if (!ok) return c.json({ error: "not found" }, 404)
  return c.json({ ok: true })
})

providerRoutes.post("/:provider/accounts/:id/unpause", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const provider = parseProvider(c.req.param("provider"))
  if (!provider) return c.json({ error: "invalid provider" }, 400)
  const row = await getAccount(c.env.DB, user.id, c.req.param("id"))
  if (!row || row.provider !== provider) return c.json({ error: "not found" }, 404)
  await clearBench(c.env, user.id, provider, row.id)
  return c.json({ ok: true })
})

providerRoutes.delete("/:provider/accounts/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const ok = await removeAccount(c.env.DB, user.id, c.req.param("id"))
  if (!ok) return c.json({ error: "not found" }, 404)
  return c.json({ ok: true })
})

/** Manual credential ingest for bootstrapping / tests (session required). */
providerRoutes.post("/:provider/accounts/import", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const provider = parseProvider(c.req.param("provider"))
  if (!provider) return c.json({ error: "invalid provider" }, 400)
  if (!c.env.TOKEN_ENCRYPTION_KEY) {
    return c.json({ error: "TOKEN_ENCRYPTION_KEY not configured" }, 500)
  }
  const body = (await c.req.json()) as {
    access_token: string
    refresh_token?: string
    expires_at?: string
    account_id?: string
    email?: string
    label?: string
    token_endpoint?: string
    client_id?: string
  }
  if (!body.access_token) return c.json({ error: "access_token required" }, 400)
  const credential: StoredCredential = {
    access_token: body.access_token,
    refresh_token: body.refresh_token ?? null,
    expires_at: body.expires_at ?? null,
    account_id: body.account_id ?? null,
    email: body.email ?? null,
    client_id: body.client_id ?? null,
    token_endpoint: body.token_endpoint ?? null,
  }
  const encrypted = await encryptJson(c.env.TOKEN_ENCRYPTION_KEY, credential)
  const row = await insertAccount(c.env.DB, {
    userId: user.id,
    provider,
    encryptedPayload: encrypted,
    label: body.label ?? body.email ?? null,
    externalAccountId: body.account_id ?? null,
    accountMetaJson: JSON.stringify({ email: body.email ?? null }),
  })
  return c.json({ ok: true, id: row.id })
})

/** Begin OAuth login — Claude uses PKCE; Codex is a server-PKCE device flow. */
providerRoutes.post("/:provider/login", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const provider = parseProvider(c.req.param("provider"))
  if (!provider) return c.json({ error: "invalid provider" }, 400)

  const loginId = newId("login")
  const expires = new Date(Date.now() + 900_000).toISOString()

  // Also opportunistically pruned here on the same path that adds new rows,
  // ahead of whatever the next daily retention sweep does (docs/logging.md).
  await c.env.DB.prepare(`DELETE FROM oauth_login_states WHERE expires_at < ?`)
    .bind(nowIso())
    .run()

  if (provider === "claude-code") {
    const { authorizationUrl, pending } = await beginClaudeAuthorization(
      c.env.CLAUDE_CODE_OAUTH_CLIENT_ID,
    )
    await c.env.DB.prepare(
      `INSERT INTO oauth_login_states (id, kind, user_id, provider, payload_json, expires_at, created_at)
       VALUES (?, 'provider', ?, ?, ?, ?, ?)`,
    )
      .bind(loginId, user.id, provider, JSON.stringify(pending), expires, nowIso())
      .run()
    return c.json({
      login_id: loginId,
      authorization_url: authorizationUrl,
      instructions:
        "Open authorization_url, approve, then paste code#state from the Anthropic callback page.",
    })
  }

  if (provider === "codex") {
    const clientId = c.env.CODEX_OAUTH_CLIENT_ID || CODEX_DEVICE_AUTH.clientId
    const deviceRes = await fetch(CODEX_DEVICE_AUTH.userCodeUrl, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        accept: "application/json",
      },
      body: JSON.stringify({ client_id: clientId }),
    })
    if (!deviceRes.ok) {
      const detail = (await deviceRes.text()).trim() || `HTTP ${deviceRes.status}`
      return c.json({ error: `device code failed: ${detail}` }, 502)
    }
    const device = (await deviceRes.json()) as {
      device_auth_id?: unknown
      user_code?: unknown
      interval?: unknown
    }
    if (typeof device.device_auth_id !== "string" || !device.device_auth_id) {
      return c.json({ error: "device code response missing device_auth_id" }, 502)
    }
    if (typeof device.user_code !== "string" || !device.user_code) {
      return c.json({ error: "device code response missing user_code" }, 502)
    }
    const parsedInterval =
      typeof device.interval === "number"
        ? device.interval
        : typeof device.interval === "string"
          ? Number(device.interval)
          : Number.NaN
    const interval = Number.isFinite(parsedInterval) ? parsedInterval : 5
    await c.env.DB.prepare(
      `INSERT INTO oauth_login_states (id, kind, user_id, provider, payload_json, expires_at, created_at)
       VALUES (?, 'provider', ?, ?, ?, ?, ?)`,
    )
      .bind(
        loginId,
        user.id,
        provider,
        JSON.stringify({
          client_id: clientId,
          device_auth_id: device.device_auth_id,
          user_code: device.user_code,
        }),
        expires,
        nowIso(),
      )
      .run()
    return c.json({
      login_id: loginId,
      user_code: device.user_code,
      verification_uri: "https://auth.openai.com/codex/device",
      interval,
    })
  }

  if (provider === "grok") {
    const clientId = c.env.GROK_OAUTH_CLIENT_ID || "b1a00492-073a-47ea-816f-4c329264a828"
    const deviceRes = await fetch("https://auth.x.ai/oauth2/device/code", {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        client_id: clientId,
        scope: "openid profile email offline_access grok-cli:access api:access",
      }),
    })
    if (!deviceRes.ok) {
      const t = await deviceRes.text()
      return c.json({ error: `device code failed: ${deviceRes.status} ${t}` }, 502)
    }
    const device = (await deviceRes.json()) as {
      device_code: string
      user_code: string
      verification_uri?: string
      verification_uri_complete?: string
      expires_in?: number
      interval?: number
    }
    await c.env.DB.prepare(
      `INSERT INTO oauth_login_states (id, kind, user_id, provider, payload_json, expires_at, created_at)
       VALUES (?, 'provider', ?, ?, ?, ?, ?)`,
    )
      .bind(
        loginId,
        user.id,
        provider,
        JSON.stringify({
          client_id: clientId,
          device_code: device.device_code,
          token_endpoint: "https://auth.x.ai/oauth2/token",
        }),
        expires,
        nowIso(),
      )
      .run()
    return c.json({
      login_id: loginId,
      user_code: device.user_code,
      verification_uri: device.verification_uri,
      verification_uri_complete: device.verification_uri_complete,
      interval: device.interval ?? 5,
    })
  }

  return c.json({ error: "unsupported" }, 400)
})

async function loadPendingLogin(
  env: HonoEnv["Bindings"],
  opts: {
    userId: string
    provider: ProviderId
    loginId: string
    oauthState?: string
  },
): Promise<{ id: string; payload_json: string; expires_at: string } | null> {
  const byId = await env.DB.prepare(
    `SELECT id, payload_json, expires_at FROM oauth_login_states
     WHERE id = ? AND user_id = ? AND provider = ?`,
  )
    .bind(opts.loginId, opts.userId, opts.provider)
    .first<{ id: string; payload_json: string; expires_at: string }>()
  if (byId && new Date(byId.expires_at).getTime() >= Date.now()) return byId

  // Fallback: match oauth_state from pasted callback (dialog may have wrong id)
  if (opts.oauthState) {
    const all = await env.DB.prepare(
      `SELECT id, payload_json, expires_at FROM oauth_login_states
       WHERE user_id = ? AND provider = ? AND kind = 'provider'`,
    )
      .bind(opts.userId, opts.provider)
      .all<{ id: string; payload_json: string; expires_at: string }>()
    for (const row of all.results ?? []) {
      if (new Date(row.expires_at).getTime() < Date.now()) continue
      try {
        const p = JSON.parse(row.payload_json) as PendingOAuth
        if (p.oauth_state === opts.oauthState) return row
      } catch {
        /* */
      }
    }
  }
  return null
}

providerRoutes.post("/:provider/login/:id/complete", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const provider = parseProvider(c.req.param("provider"))
  if (!provider) return c.json({ error: "invalid provider" }, 400)
  const loginId = c.req.param("id")
  const body = (await c.req.json().catch(() => ({}))) as { code?: string; value?: string }
  const raw = (body.code || body.value || "").trim()

  if (provider === "claude-code") {
    try {
      const { code, state } = parseCodeHashState(raw)
      const stateRow = await loadPendingLogin(c.env, {
        userId: user.id,
        provider,
        loginId,
        oauthState: state,
      })
      if (!stateRow) return c.json({ error: "login expired; start again" }, 400)
      const pending = JSON.parse(stateRow.payload_json) as PendingOAuth
      const tok = await exchangeClaudeCode({ code, returnedState: state, pending })
      const identity = await fetchClaudeIdentity(tok.access_token)
      const label = pickAccountLabel({
        email: identity.email,
        displayName: identity.displayName,
        fallback: "claude",
      })
      const credential: StoredCredential = {
        access_token: tok.access_token,
        refresh_token: tok.refresh_token ?? null,
        expires_at: tok.expires_in
          ? new Date(Date.now() + tok.expires_in * 1000).toISOString()
          : null,
        client_id: pending.client_id,
        email: identity.email,
      }
      const encrypted = await encryptJson(c.env.TOKEN_ENCRYPTION_KEY, credential)
      const row = await insertAccount(c.env.DB, {
        userId: user.id,
        provider,
        encryptedPayload: encrypted,
        label,
        accountMetaJson: JSON.stringify({
          email: identity.email,
          display_name: identity.displayName,
        }),
      })
      await c.env.DB.prepare(`DELETE FROM oauth_login_states WHERE id = ?`)
        .bind(stateRow.id)
        .run()
      return c.json({ ok: true, token_id: row.id, label })
    } catch (e) {
      return c.json({ error: e instanceof Error ? e.message : "complete failed" }, 400)
    }
  }

  if (provider === "codex") {
    try {
      const stateRow = await loadPendingLogin(c.env, {
        userId: user.id,
        provider,
        loginId,
      })
      if (!stateRow) return c.json({ error: "login expired; start again" }, 400)
      const pending = JSON.parse(stateRow.payload_json) as {
        client_id?: unknown
        device_auth_id?: unknown
        user_code?: unknown
      }
      if (
        typeof pending.client_id !== "string" ||
        typeof pending.device_auth_id !== "string" ||
        typeof pending.user_code !== "string"
      ) {
        return c.json({ error: "invalid Codex device login state; start again" }, 400)
      }
      const tokenRes = await fetch(CODEX_DEVICE_AUTH.deviceTokenUrl, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          accept: "application/json",
        },
        body: JSON.stringify({
          device_auth_id: pending.device_auth_id,
          user_code: pending.user_code,
        }),
      })
      const tokenText = await tokenRes.text()
      let deviceToken: {
        authorization_code?: unknown
        code_verifier?: unknown
        error?: unknown
      } = {}
      try {
        deviceToken = JSON.parse(tokenText) as typeof deviceToken
      } catch {
        /* The pending 403/404 responses may not be JSON. */
      }
      const deviceError = typeof deviceToken.error === "string" ? deviceToken.error : null
      if (deviceError === "authorization_pending" || deviceError === "slow_down") {
        return c.json({ error: deviceError }, 400)
      }
      if (tokenRes.status === 403 || tokenRes.status === 404) {
        return c.json({ error: `token ${tokenRes.status}` }, 400)
      }
      if (!tokenRes.ok) {
        const detail = deviceError || tokenText.trim() || `HTTP ${tokenRes.status}`
        return c.json({ error: `Codex device token failed: ${detail}` }, 400)
      }
      if (typeof deviceToken.authorization_code !== "string" || !deviceToken.authorization_code) {
        return c.json({ error: "Codex device token response missing authorization_code" }, 400)
      }
      const codeVerifier =
        typeof deviceToken.code_verifier === "string" && deviceToken.code_verifier
      if (!codeVerifier) {
        return c.json({ error: "Codex device token response missing code_verifier" }, 400)
      }
      const exchangeRes = await fetch(CODEX_DEVICE_AUTH.tokenUrl, {
        method: "POST",
        headers: {
          "content-type": "application/x-www-form-urlencoded",
          accept: "application/json",
        },
        body: new URLSearchParams({
          grant_type: "authorization_code",
          client_id: pending.client_id,
          code: deviceToken.authorization_code,
          code_verifier: codeVerifier,
          redirect_uri: CODEX_DEVICE_AUTH.redirectUri,
        }),
      })
      if (!exchangeRes.ok) {
        const detail = (await exchangeRes.text()).trim() || `HTTP ${exchangeRes.status}`
        return c.json({ error: `Codex OAuth token exchange failed: ${detail}` }, 400)
      }
      const tok = (await exchangeRes.json()) as {
        access_token: string
        refresh_token?: string
        expires_in?: number
      }
      const accountId = extractChatgptAccountId(tok.access_token)
      const identity = await fetchCodexIdentity(tok.access_token, accountId)
      const label = pickAccountLabel({
        email: identity.email,
        displayName: identity.displayName,
        fallback: accountId ? `codex:${accountId.slice(0, 8)}` : "codex",
      })
      const credential: StoredCredential = {
        access_token: tok.access_token,
        refresh_token: tok.refresh_token ?? null,
        expires_at:
          extractJwtExpiryIso(tok.access_token) ??
          (tok.expires_in
            ? new Date(Date.now() + tok.expires_in * 1000).toISOString()
            : null),
        account_id: accountId,
        client_id: pending.client_id,
        email: identity.email,
      }
      const encrypted = await encryptJson(c.env.TOKEN_ENCRYPTION_KEY, credential)
      const row = await insertAccount(c.env.DB, {
        userId: user.id,
        provider,
        encryptedPayload: encrypted,
        externalAccountId: accountId,
        label,
        accountMetaJson: JSON.stringify({
          email: identity.email,
          plan_type: identity.plan,
          account_id: accountId,
        }),
      })
      await c.env.DB.prepare(`DELETE FROM oauth_login_states WHERE id = ?`)
        .bind(stateRow.id)
        .run()
      return c.json({ ok: true, token_id: row.id, label })
    } catch (e) {
      return c.json({ error: e instanceof Error ? e.message : "complete failed" }, 400)
    }
  }

  if (provider === "grok") {
    const stateRow = await loadPendingLogin(c.env, {
      userId: user.id,
      provider,
      loginId,
    })
    if (!stateRow) return c.json({ error: "login expired; start again" }, 400)
    const payload = JSON.parse(stateRow.payload_json) as Record<string, string>
    const tokenRes = await fetch(payload.token_endpoint || "https://auth.x.ai/oauth2/token", {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "urn:ietf:params:oauth:grant-type:device_code",
        device_code: payload.device_code!,
        client_id: payload.client_id!,
      }),
    })
    if (!tokenRes.ok) {
      return c.json({ error: `token ${tokenRes.status}`, detail: await tokenRes.text() }, 400)
    }
    const tok = (await tokenRes.json()) as {
      access_token: string
      refresh_token?: string
      expires_in?: number
    }
    const identity = await fetchGrokIdentity(tok.access_token)
    const label = pickAccountLabel({
      email: identity.email,
      displayName: identity.displayName,
      fallback: "grok",
    })
    const credential: StoredCredential = {
      access_token: tok.access_token,
      refresh_token: tok.refresh_token ?? null,
      expires_at: tok.expires_in
        ? new Date(Date.now() + tok.expires_in * 1000).toISOString()
        : null,
      client_id: payload.client_id,
      token_endpoint: payload.token_endpoint || "https://auth.x.ai/oauth2/token",
      email: identity.email,
    }
    const encrypted = await encryptJson(c.env.TOKEN_ENCRYPTION_KEY, credential)
    const row = await insertAccount(c.env.DB, {
      userId: user.id,
      provider,
      encryptedPayload: encrypted,
      label,
      accountMetaJson: JSON.stringify({
        email: identity.email,
        display_name: identity.displayName,
      }),
    })
    await c.env.DB.prepare(`DELETE FROM oauth_login_states WHERE id = ?`).bind(stateRow.id).run()
    return c.json({ ok: true, token_id: row.id, label })
  }

  return c.json({ error: "unsupported" }, 400)
})
