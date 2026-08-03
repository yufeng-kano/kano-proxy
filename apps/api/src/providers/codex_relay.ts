/**
 * Codex egress relay client — mints Cloud Run IAM ID tokens and applies the
 * tri-state guard from docs/codex-relay.md "Wire contract" / "Auth: Cloud
 * Run IAM". The relay app itself lives in apps/relay (a separate Deno app,
 * outside this Worker's dependency graph); this module is everything the
 * Worker side needs to talk to it.
 */

import type { Env } from "../env"

const DIRECT_BASE = "https://chatgpt.com/backend-api"
const TOKEN_EXCHANGE_URL = "https://oauth2.googleapis.com/token"

/** Refresh once fewer than this many ms of validity remain (docs: "<5 minutes"). */
const ID_TOKEN_REFRESH_MARGIN_MS = 5 * 60 * 1000
/** `exp` claim undecodable — assume the standard 1h token, minus a safety margin. */
const ID_TOKEN_FALLBACK_TTL_MS = 3500 * 1000

export type CodexUpstream = {
  /** `${CODEX_RELAY_URL}/backend-api` when the relay is enabled, else the direct chatgpt.com base. */
  base: string
  relay: boolean
  /** Resolves to the extra headers this call needs — the IAM header when relayed, empty otherwise. */
  headers(): Promise<Record<string, string>>
}

/**
 * Relay is enabled only when BOTH `CODEX_RELAY_URL` and `CODEX_RELAY_SA_KEY`
 * are set — either alone leaves codex traffic going direct to chatgpt.com,
 * today's (403-walled) behavior. See docs/codex-relay.md "Worker configuration".
 */
export function codexUpstream(env: Env): CodexUpstream {
  if (env.CODEX_RELAY_URL && env.CODEX_RELAY_SA_KEY) {
    const base = `${env.CODEX_RELAY_URL.replace(/\/+$/, "")}/backend-api`
    return {
      base,
      relay: true,
      headers: async () => ({ "x-serverless-authorization": `Bearer ${await mintIdToken(env)}` }),
    }
  }
  return {
    base: DIRECT_BASE,
    relay: false,
    headers: async () => ({}),
  }
}

type ServiceAccountKey = { client_email: string; private_key: string }

function parseServiceAccountKey(raw: string): ServiceAccountKey {
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    throw new Error("CODEX_RELAY_SA_KEY is not valid JSON")
  }
  const sa = parsed as Partial<ServiceAccountKey> | null
  if (!sa || typeof sa.client_email !== "string" || typeof sa.private_key !== "string") {
    throw new Error("CODEX_RELAY_SA_KEY is missing client_email or private_key")
  }
  return { client_email: sa.client_email, private_key: sa.private_key }
}

/** Per-isolate cache keyed by relay origin — one mint per isolate per hour (docs: "Minting"). */
const idTokenCache = new Map<string, { idToken: string; expiresAtMs: number }>()

export function resetCodexRelayCacheForTests(): void {
  idTokenCache.clear()
}

/**
 * Forces the next `mintIdToken` for this relay origin to skip the cache —
 * the tri-state guard's single retry after a marker-less 401/403 needs this,
 * since re-reading a still-cached-but-rejected token would just repeat the
 * same failure.
 */
export function invalidateCodexRelayToken(origin: string): void {
  idTokenCache.delete(origin)
}

/**
 * Mint (or reuse) a Google-signed ID token whose audience is the relay
 * origin, per docs/codex-relay.md "Auth: Cloud Run IAM". Cloud Run checks
 * this in `X-Serverless-Authorization` before the container is invoked —
 * `Authorization` stays free for the upstream ChatGPT bearer.
 */
export async function mintIdToken(env: Env): Promise<string> {
  if (!env.CODEX_RELAY_URL || !env.CODEX_RELAY_SA_KEY) {
    throw new Error("codex relay is not configured")
  }
  const origin = new URL(env.CODEX_RELAY_URL).origin
  const cached = idTokenCache.get(origin)
  if (cached && cached.expiresAtMs - Date.now() > ID_TOKEN_REFRESH_MARGIN_MS) {
    return cached.idToken
  }

  const sa = parseServiceAccountKey(env.CODEX_RELAY_SA_KEY)
  const assertion = await signAssertion(sa, origin)
  const idToken = await exchangeAssertion(assertion)
  const expiresAtMs = decodeJwtExpMs(idToken) ?? Date.now() + ID_TOKEN_FALLBACK_TTL_MS
  idTokenCache.set(origin, { idToken, expiresAtMs })
  return idToken
}

/** `{alg:RS256,typ:JWT}` + claims per docs, signed with the SA private key via WebCrypto. */
async function signAssertion(sa: ServiceAccountKey, targetAudience: string): Promise<string> {
  const now = Math.floor(Date.now() / 1000)
  const header = { alg: "RS256", typ: "JWT" }
  const claims = {
    iss: sa.client_email,
    sub: sa.client_email,
    aud: TOKEN_EXCHANGE_URL,
    iat: now,
    exp: now + 3600,
    target_audience: targetAudience,
  }
  const signingInput = `${base64UrlJson(header)}.${base64UrlJson(claims)}`
  const key = await crypto.subtle.importKey(
    "pkcs8",
    pkcs8FromPem(sa.private_key),
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["sign"],
  )
  const signature = await crypto.subtle.sign(
    "RSASSA-PKCS1-v1_5",
    key,
    new TextEncoder().encode(signingInput),
  )
  return `${signingInput}.${base64Url(new Uint8Array(signature))}`
}

async function exchangeAssertion(assertion: string): Promise<string> {
  const res = await fetch(TOKEN_EXCHANGE_URL, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer",
      assertion,
    }),
  })
  if (!res.ok) throw new Error(`codex relay token exchange failed: ${res.status}`)
  const json = (await res.json()) as { id_token?: string }
  if (!json.id_token) throw new Error("codex relay token exchange response missing id_token")
  return json.id_token
}

/**
 * Send one call through the relay, applying the tri-state guard from
 * docs/codex-relay.md "Wire contract": `x-relay-upstream` passes through
 * untouched (normal upstream semantics, bench included); `x-relay-fault`
 * becomes a 502 that never propagates the relay's own fault status; neither
 * marker on a 401/403 gets one re-mint + retry, and any marker-less outcome
 * — including the retry, and any other status such as 429/500 — becomes a
 * 502 too. A relay-side problem must never surface as 401/402/403/429,
 * which would bench the pool account for something that isn't its fault.
 */
export async function relayFetch(
  upstream: CodexUpstream,
  url: string,
  init: RequestInit,
): Promise<Response> {
  if (!upstream.relay) return fetch(url, init)

  const attempt = async (): Promise<Response> => {
    const headers = new Headers(init.headers)
    for (const [name, value] of Object.entries(await upstream.headers())) {
      headers.set(name, value)
    }
    // init.body at every call site here is a string, so it is safe to reuse
    // across this one possible retry (no locked-stream concerns).
    return fetch(url, { ...init, headers })
  }

  let res = await attempt()
  if (res.headers.has("x-relay-upstream")) return res
  if (res.headers.has("x-relay-fault")) return relayFaultResponse(res)

  if (res.status === 401 || res.status === 403) {
    invalidateCodexRelayToken(new URL(url).origin)
    res = await attempt()
    if (res.headers.has("x-relay-upstream")) return res
    if (res.headers.has("x-relay-fault")) return relayFaultResponse(res)
  }

  return Response.json({ error: { type: "relay_unavailable", status: res.status } }, { status: 502 })
}

function relayFaultResponse(res: Response): Response {
  return Response.json(
    { error: { type: "relay_fault", reason: res.headers.get("x-relay-fault") } },
    { status: 502 },
  )
}

function base64Url(bytes: Uint8Array): string {
  let s = ""
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]!)
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
}

function base64UrlJson(value: unknown): string {
  return base64Url(new TextEncoder().encode(JSON.stringify(value)))
}

/** GCP SA JSON keys ship PKCS8 PEM — strip the armor and decode straight to DER. */
function pkcs8FromPem(pem: string): ArrayBuffer {
  const base64 = pem
    .replace(/-----BEGIN PRIVATE KEY-----/, "")
    .replace(/-----END PRIVATE KEY-----/, "")
    .replace(/\s+/g, "")
  const bin = atob(base64)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return bytes.buffer
}

function decodeJwtExpMs(token: string): number | null {
  try {
    const payload = token.split(".")[1]
    if (!payload) return null
    const json = JSON.parse(atob(payload.replace(/-/g, "+").replace(/_/g, "/"))) as { exp?: number }
    return typeof json.exp === "number" ? json.exp * 1000 : null
  } catch {
    return null
  }
}
