/**
 * Provider OAuth (Claude Code / Codex) aligned with lincy-agent.
 * Both require PKCE S256.
 */

import { buildPkcePair, buildStateToken } from "./pkce"

export const CLAUDE_OAUTH = {
  clientId: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
  authorizeUrl: "https://claude.ai/oauth/authorize",
  tokenUrl: "https://console.anthropic.com/v1/oauth/token",
  redirectUri: "https://console.anthropic.com/oauth/code/callback",
  scope: "org:create_api_key user:profile user:inference",
} as const

/** Must match the public Codex CLI OAuth client registration (lincy default). */
export const CODEX_OAUTH = {
  clientId: "app_EMoamEEZ73f0CkXaXp7hrann",
  authorizeUrl: "https://auth.openai.com/oauth/authorize",
  tokenUrl: "https://auth.openai.com/oauth/token",
  /** Registered redirect — do not change unless you own a custom OAuth app. */
  redirectUri: "http://localhost:1455/auth/callback",
  scope: "openid profile email offline_access",
} as const

export type PendingOAuth = {
  client_id: string
  code_verifier: string
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

export async function beginCodexAuthorization(clientId?: string): Promise<{
  authorizationUrl: string
  pending: PendingOAuth
}> {
  const { codeVerifier, codeChallenge } = await buildPkcePair()
  const oauthState = buildStateToken()
  const cid = clientId || CODEX_OAUTH.clientId
  // Match lincy: extra Codex CLI flags + PKCE
  const params = new URLSearchParams({
    response_type: "code",
    client_id: cid,
    redirect_uri: CODEX_OAUTH.redirectUri,
    scope: CODEX_OAUTH.scope,
    code_challenge: codeChallenge,
    code_challenge_method: "S256",
    id_token_add_organizations: "true",
    codex_cli_simplified_flow: "true",
    state: oauthState,
    originator: "codex_cli_rs",
  })
  return {
    authorizationUrl: `${CODEX_OAUTH.authorizeUrl}?${params}`,
    pending: {
      client_id: cid,
      code_verifier: codeVerifier,
      oauth_state: oauthState,
      redirect_uri: CODEX_OAUTH.redirectUri,
    },
  }
}

export async function exchangeCodexCode(opts: {
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
  const res = await fetch(CODEX_OAUTH.tokenUrl, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "authorization_code",
      client_id: opts.pending.client_id,
      code: opts.code,
      code_verifier: opts.pending.code_verifier,
      redirect_uri: opts.pending.redirect_uri,
    }),
  })
  if (!res.ok) {
    const detail = (await res.text()).trim() || `HTTP ${res.status}`
    throw new Error(`Codex OAuth token exchange failed: ${detail}`)
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
