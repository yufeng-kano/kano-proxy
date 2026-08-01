import type { Env } from "../env"
import { newId, nowIso } from "../utils/id"

const GOOGLE_AUTH = "https://accounts.google.com/o/oauth2/v2/auth"
const GOOGLE_TOKEN = "https://oauth2.googleapis.com/token"
const GOOGLE_USERINFO = "https://openidconnect.googleapis.com/v1/userinfo"

export async function beginGoogleLogin(env: Env): Promise<{ url: string; state: string }> {
  const clientId = env.GOOGLE_CLIENT_ID
  if (!clientId) throw new Error("GOOGLE_CLIENT_ID is not configured")
  const state = newId("gstate")
  const expires = new Date(Date.now() + 900_000).toISOString()
  await env.DB.prepare(
    `INSERT INTO oauth_login_states (id, kind, user_id, provider, payload_json, expires_at, created_at)
     VALUES (?, 'google', NULL, NULL, ?, ?, ?)`,
  )
    .bind(state, JSON.stringify({}), expires, nowIso())
    .run()

  const params = new URLSearchParams({
    client_id: clientId,
    redirect_uri: env.GOOGLE_REDIRECT_URI,
    response_type: "code",
    scope: "openid email profile",
    state,
    access_type: "online",
    prompt: "select_account",
  })
  return { url: `${GOOGLE_AUTH}?${params}`, state }
}

export async function completeGoogleLogin(
  env: Env,
  code: string,
  state: string,
): Promise<{ sub: string; email: string; name?: string; picture?: string }> {
  const row = await env.DB.prepare(
    `SELECT * FROM oauth_login_states WHERE id = ? AND kind = 'google'`,
  )
    .bind(state)
    .first<{ expires_at: string }>()
  if (!row || new Date(row.expires_at).getTime() < Date.now()) {
    throw new Error("Invalid or expired OAuth state")
  }
  await env.DB.prepare(`DELETE FROM oauth_login_states WHERE id = ?`).bind(state).run()

  if (!env.GOOGLE_CLIENT_ID || !env.GOOGLE_CLIENT_SECRET) {
    throw new Error("Google OAuth is not configured")
  }

  const tokenRes = await fetch(GOOGLE_TOKEN, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      code,
      client_id: env.GOOGLE_CLIENT_ID,
      client_secret: env.GOOGLE_CLIENT_SECRET,
      redirect_uri: env.GOOGLE_REDIRECT_URI,
      grant_type: "authorization_code",
    }),
  })
  if (!tokenRes.ok) {
    throw new Error(`Google token exchange failed: ${tokenRes.status}`)
  }
  const tokenJson = (await tokenRes.json()) as { access_token: string }
  const profileRes = await fetch(GOOGLE_USERINFO, {
    headers: { authorization: `Bearer ${tokenJson.access_token}` },
  })
  if (!profileRes.ok) {
    throw new Error(`Google userinfo failed: ${profileRes.status}`)
  }
  const profile = (await profileRes.json()) as {
    sub: string
    email: string
    name?: string
    picture?: string
  }
  if (!profile.sub || !profile.email) throw new Error("Google profile missing sub/email")
  return profile
}
