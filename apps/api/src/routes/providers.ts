import { Hono } from "hono"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import {
  beginClaudeAuthorization,
  beginCodexAuthorization,
  exchangeClaudeCode,
  exchangeCodexCode,
  extractChatgptAccountId,
  extractJwtExpiryIso,
  type PendingOAuth,
} from "../auth/provider_oauth"
import { parseCodeHashState, parseCodexCallbackValue } from "../auth/pkce"
import {
  insertAccount,
  listAccounts,
  promoteAccount,
  removeAccount,
  updateAccountIdentity,
} from "../db/accounts"
import {
  fetchClaudeIdentity,
  fetchCodexIdentity,
  fetchGrokIdentity,
  pickAccountLabel,
} from "../providers/identity"
import { readUsageCache, writeUsageCache } from "../pool/usage_cache"
import { encryptJson } from "../crypto/token_crypto"
import { isProviderId, type ProviderId } from "../env"
import { isBenched } from "../pool/bench"
import type { StoredCredential } from "../pool/acquire"
import { getAdapter } from "../providers"
import { newId, nowIso } from "../utils/id"
import { decryptJson } from "../crypto/token_crypto"

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

  // ?refresh=true bypasses 90s KV usage cache (manual refresh). Default hits cache.
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
      try {
        const cached = force
          ? null
          : await readUsageCache(c.env, user.id, provider, row.id)
        if (cached) {
          usage = { windows: cached.windows }
          accountMeta = { ...accountMeta, ...cached.account }
          // Keep upstream stale flag from when the snap was fetched (not "from cache")
          stale = cached.stale
          error = cached.error
          if (
            status !== "benched" &&
            error &&
            !cached.windows.length &&
            !cached.edgeBlocked &&
            /401|invalid.?token|unauthorized/i.test(error)
          ) {
            status = "unusable"
          }
        } else {
          const cred = await decryptJson<StoredCredential>(
            c.env.TOKEN_ENCRYPTION_KEY,
            row.encrypted_payload,
          )
          const snap = await adapter.fetchUsage(c.env, { row, credential: cred })
          usage = { windows: snap.windows }
          accountMeta = { ...accountMeta, ...snap.account }
          stale = !!snap.stale
          error = snap.error ?? null
          await writeUsageCache(c.env, user.id, provider, row.id, {
            windows: snap.windows,
            account: snap.account,
            stale: !!snap.stale,
            error: snap.error ?? null,
            edgeBlocked: snap.edgeBlocked,
          })
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
          // Prefer upstream email / username as stable pool label
          const email =
            typeof snap.account.email === "string" ? snap.account.email : null
          const display =
            typeof snap.account.display_name === "string"
              ? snap.account.display_name
              : typeof snap.account.username === "string"
                ? snap.account.username
                : null
          const better = pickAccountLabel({
            email,
            displayName: display,
            fallback: row.label || row.id,
          })
          if (better && better !== row.label) {
            row.label = better
            await updateAccountIdentity(c.env.DB, row.id, {
              label: better,
              accountMetaJson: JSON.stringify(accountMeta),
            })
          } else if (accountMeta) {
            await updateAccountIdentity(c.env.DB, row.id, {
              accountMetaJson: JSON.stringify(accountMeta),
            })
          }
        }
      } catch (e) {
        error = e instanceof Error ? e.message : "usage failed"
        stale = true
      }
    }

    const displayLabel =
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

  return c.json({ available: true, accounts, models: [], error: null })
})

providerRoutes.post("/:provider/accounts/:id/promote", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const ok = await promoteAccount(c.env.DB, user.id, c.req.param("id"))
  if (!ok) return c.json({ error: "not found" }, 404)
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

/** Begin OAuth login — PKCE required (Claude + Codex). */
providerRoutes.post("/:provider/login", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const provider = parseProvider(c.req.param("provider"))
  if (!provider) return c.json({ error: "invalid provider" }, 400)

  const loginId = newId("login")
  const expires = new Date(Date.now() + 900_000).toISOString()

  // No scheduled sweeper for this table — prune expired rows opportunistically
  // on the same path that adds new ones.
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
    // Public Codex client only accepts redirect_uri=http://localhost:1455/auth/callback
    // (lincy). Browser will fail to connect there — copy the full URL from the address bar
    // (or code#state) and paste back into complete.
    const { authorizationUrl, pending } = await beginCodexAuthorization(c.env.CODEX_OAUTH_CLIENT_ID)
    await c.env.DB.prepare(
      `INSERT INTO oauth_login_states (id, kind, user_id, provider, payload_json, expires_at, created_at)
       VALUES (?, 'provider', ?, ?, ?, ?, ?)`,
    )
      .bind(loginId, user.id, provider, JSON.stringify(pending), expires, nowIso())
      .run()
    return c.json({
      login_id: loginId,
      authorization_url: authorizationUrl,
      redirect_uri: pending.redirect_uri,
      instructions:
        "Open authorization_url, sign in. When the browser lands on localhost:1455 (may fail to load), copy the full URL from the address bar and paste it to complete.",
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
      const { code, state } = parseCodexCallbackValue(raw)
      const stateRow = await loadPendingLogin(c.env, {
        userId: user.id,
        provider,
        loginId,
        oauthState: state,
      })
      if (!stateRow) {
        return c.json(
          {
            error:
              "login expired or state not found; click Start OAuth again, then paste the new callback URL immediately",
          },
          400,
        )
      }
      const pending = JSON.parse(stateRow.payload_json) as PendingOAuth
      const tok = await exchangeCodexCode({ code, returnedState: state, pending })
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
