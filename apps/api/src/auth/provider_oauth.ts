/**
 * Provider OAuth helpers. Claude Code uses browser PKCE; Codex device PKCE is
 * server-side; Antigravity is a plain confidential-client code flow.
 */

import { buildPkcePair, buildStateToken } from "./pkce"

export const CLAUDE_OAUTH = {
  clientId: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
  authorizeUrl: "https://claude.ai/oauth/authorize",
  tokenUrl: "https://console.anthropic.com/v1/oauth/token",
  redirectUri: "https://console.anthropic.com/oauth/code/callback",
  scope: "org:create_api_key user:profile user:inference",
} as const

/** Must match the public Codex CLI OAuth client registration. */
export const CODEX_DEVICE_AUTH = {
  clientId: "app_EMoamEEZ73f0CkXaXp7hrann",
  userCodeUrl: "https://auth.openai.com/api/accounts/deviceauth/usercode",
  deviceTokenUrl: "https://auth.openai.com/api/accounts/deviceauth/token",
  tokenUrl: "https://auth.openai.com/oauth/token",
  redirectUri: "https://auth.openai.com/deviceauth/callback",
} as const

/**
 * Antigravity is a *confidential* Google OAuth client: no PKCE, and the client
 * secret is required on both the code exchange and every refresh.
 *
 * **The credential pair is not in this repo, on purpose.** Unlike the other
 * three providers — whose public client ids are plain identifiers with no
 * secret — this one needs a client *secret*, and committing an OAuth client
 * secret is forbidden here whatever its provenance (see the project rules and
 * docs/auth.md § Antigravity). Both halves come from
 * `ANTIGRAVITY_OAUTH_CLIENT_ID` / `ANTIGRAVITY_OAUTH_CLIENT_SECRET`; with
 * either unset the provider refuses to start a login rather than falling back
 * to something baked in.
 *
 * Endpoints, scopes and the callback port below are configuration, not
 * secrets, and match CLIProxyAPI `internal/auth/antigravity/constants.go`.
 */
export const ANTIGRAVITY_OAUTH = {
  authorizeUrl: "https://accounts.google.com/o/oauth2/v2/auth",
  tokenUrl: "https://oauth2.googleapis.com/token",
  /**
   * The only redirect this client has registered. Verified 2026-08-22: Google
   * rejects any other value for this client id with `invalid_request` ("doesn't
   * comply with Google's OAuth 2.0 policy"), so the proxy cannot host its own
   * callback and the user pastes the code back instead (docs/auth.md).
   */
  redirectUri: "http://localhost:51121/oauth-callback",
  scope: [
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
  ].join(" "),
} as const

export type PendingOAuth = {
  client_id: string
  code_verifier: string
  oauth_state: string
  redirect_uri: string
}

/** Stored between Antigravity login start and complete — no verifier, this client has no PKCE. */
export type PendingAntigravityOAuth = {
  client_id: string
  oauth_state: string
  redirect_uri: string
}

export async function beginClaudeAuthorization(clientId?: string): Promise<{
  authorizationUrl: string
  pending: PendingOAuth
}> {
  const { codeVerifier, codeChallenge } = await buildPkcePair()
  const oauthState = buildStateToken()
  const cid = clientId || CLAUDE_OAUTH.clientId
  const params = new URLSearchParams({
    code: "true",
    client_id: cid,
    response_type: "code",
    redirect_uri: CLAUDE_OAUTH.redirectUri,
    scope: CLAUDE_OAUTH.scope,
    code_challenge: codeChallenge,
    code_challenge_method: "S256",
    state: oauthState,
  })
  return {
    authorizationUrl: `${CLAUDE_OAUTH.authorizeUrl}?${params}`,
    pending: {
      client_id: cid,
      code_verifier: codeVerifier,
      oauth_state: oauthState,
      redirect_uri: CLAUDE_OAUTH.redirectUri,
    },
  }
}

export async function exchangeClaudeCode(opts: {
  code: string
  returnedState: string
  pending: PendingOAuth
}): Promise<{
  access_token: string
  refresh_token?: string
  expires_in?: number
}> {
  if (opts.returnedState !== opts.pending.oauth_state) {
    throw new Error("Authorization state mismatch. Restart the login flow.")
  }
  const res = await fetch(CLAUDE_OAUTH.tokenUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      grant_type: "authorization_code",
      code: opts.code,
      state: opts.pending.oauth_state,
      client_id: opts.pending.client_id,
      code_verifier: opts.pending.code_verifier,
      redirect_uri: opts.pending.redirect_uri,
    }),
  })
  if (!res.ok) {
    const detail = (await res.text()).trim() || `HTTP ${res.status}`
    throw new Error(`Claude OAuth token exchange failed: ${detail}`)
  }
  return (await res.json()) as {
    access_token: string
    refresh_token?: string
    expires_in?: number
  }
}

/**
 * `null` unless the operator configured **both** halves — a client id without
 * its secret cannot complete the exchange, so half a pair is treated as none
 * rather than failing later with an opaque Google error.
 */
export function antigravityOAuthClient(env: {
  ANTIGRAVITY_OAUTH_CLIENT_ID?: string
  ANTIGRAVITY_OAUTH_CLIENT_SECRET?: string
}): { clientId: string; clientSecret: string } | null {
  const clientId = env.ANTIGRAVITY_OAUTH_CLIENT_ID?.trim()
  const clientSecret = env.ANTIGRAVITY_OAUTH_CLIENT_SECRET?.trim()
  if (!clientId || !clientSecret) return null
  return { clientId, clientSecret }
}

export function beginAntigravityAuthorization(clientId: string): {
  authorizationUrl: string
  pending: PendingAntigravityOAuth
} {
  const oauthState = buildStateToken()
  const cid = clientId
  const params = new URLSearchParams({
    access_type: "offline",
    client_id: cid,
    prompt: "consent",
    redirect_uri: ANTIGRAVITY_OAUTH.redirectUri,
    response_type: "code",
    scope: ANTIGRAVITY_OAUTH.scope,
    state: oauthState,
  })
  return {
    authorizationUrl: `${ANTIGRAVITY_OAUTH.authorizeUrl}?${params}`,
    pending: {
      client_id: cid,
      oauth_state: oauthState,
      redirect_uri: ANTIGRAVITY_OAUTH.redirectUri,
    },
  }
}

/**
 * The user's browser lands on `http://localhost:51121/oauth-callback?...`,
 * which nothing serves, so they paste either the whole failed URL out of the
 * address bar or just the `code` value. Both are accepted; `state` comes back
 * only in the URL form, and its absence is reported rather than waved through
 * — a bare code carries no CSRF binding of its own.
 */
export function parseAntigravityCallback(raw: string): { code: string; state: string | null } {
  const value = raw.trim()
  if (!value) throw new Error("Paste the code (or the full callback URL) from the browser.")
  if (/^https?:\/\//i.test(value)) {
    let url: URL
    try {
      url = new URL(value)
    } catch {
      throw new Error("That does not look like the callback URL. Paste it again.")
    }
    const error = url.searchParams.get("error")
    if (error) throw new Error(`Google returned "${error}". Restart the login flow.`)
    const code = url.searchParams.get("code")
    if (!code) throw new Error("The callback URL carries no code. Restart the login flow.")
    return { code, state: url.searchParams.get("state") }
  }
  return { code: value, state: null }
}

export async function exchangeAntigravityCode(opts: {
  code: string
  /** `null` when the user pasted a bare code; only a returned state is checked. */
  returnedState: string | null
  pending: PendingAntigravityOAuth
  clientSecret: string
}): Promise<{
  access_token: string
  refresh_token?: string
  expires_in?: number
}> {
  if (opts.returnedState !== null && opts.returnedState !== opts.pending.oauth_state) {
    throw new Error("Authorization state mismatch. Restart the login flow.")
  }
  const res = await fetch(ANTIGRAVITY_OAUTH.tokenUrl, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "authorization_code",
      code: opts.code,
      client_id: opts.pending.client_id,
      client_secret: opts.clientSecret,
      redirect_uri: opts.pending.redirect_uri,
    }),
  })
  if (!res.ok) {
    const detail = (await res.text()).trim() || `HTTP ${res.status}`
    throw new Error(`Antigravity OAuth token exchange failed: ${detail}`)
  }
  return (await res.json()) as {
    access_token: string
    refresh_token?: string
    expires_in?: number
  }
}

export function extractChatgptAccountId(accessToken: string): string | null {
  try {
    const mid = accessToken.split(".")[1]
    if (!mid) return null
    const padded = mid + "=".repeat((4 - (mid.length % 4)) % 4)
    const json = JSON.parse(atob(padded.replace(/-/g, "+").replace(/_/g, "/"))) as {
      "https://api.openai.com/auth"?: { chatgpt_account_id?: string }
      exp?: number
    }
    return json["https://api.openai.com/auth"]?.chatgpt_account_id ?? null
  } catch {
    return null
  }
}

export function extractJwtExpiryIso(accessToken: string): string | null {
  try {
    const mid = accessToken.split(".")[1]
    if (!mid) return null
    const padded = mid + "=".repeat((4 - (mid.length % 4)) % 4)
    const json = JSON.parse(atob(padded.replace(/-/g, "+").replace(/_/g, "/"))) as {
      exp?: number
    }
    if (typeof json.exp !== "number") return null
    return new Date(json.exp * 1000).toISOString()
  } catch {
    return null
  }
}
