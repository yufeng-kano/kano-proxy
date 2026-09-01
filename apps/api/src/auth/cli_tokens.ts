/**
 * CLI device tokens (docs/cli.md § Device auth).
 *
 * Access tokens are stateless: `base64url(claims JSON).hmac-sha256-hex`,
 * signed with the dedicated CLI_TOKEN_SECRET (same signing pattern as the
 * session cookie) and verified without a D1 read. Refresh tokens are random
 * 32 bytes, stored hashed, rotating on every use — see db/cli.ts for the
 * rotation/theft semantics.
 */

import { timingSafeEqual } from "./session"

export const ACCESS_TOKEN_TTL_S = 3600

export type CliTokenClaims = {
  user_id: string
  device_id: string
  /** Unix seconds. */
  exp: number
}

async function hmacHex(secret: string, data: string): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  )
  const sig = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(data))
  return [...new Uint8Array(sig)].map((b) => b.toString(16).padStart(2, "0")).join("")
}

function base64Url(bytes: Uint8Array): string {
  let s = ""
  for (const b of bytes) s += String.fromCharCode(b)
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
}

function base64UrlDecode(s: string): string | null {
  try {
    return atob(s.replace(/-/g, "+").replace(/_/g, "/"))
  } catch {
    return null
  }
}

export async function mintAccessToken(
  secret: string,
  input: { userId: string; deviceId: string },
  now: number = Date.now(),
): Promise<{ token: string; expiresIn: number }> {
  const claims: CliTokenClaims = {
    user_id: input.userId,
    device_id: input.deviceId,
    exp: Math.floor(now / 1000) + ACCESS_TOKEN_TTL_S,
  }
  const payload = base64Url(new TextEncoder().encode(JSON.stringify(claims)))
  const sig = await hmacHex(secret, payload)
  return { token: `${payload}.${sig}`, expiresIn: ACCESS_TOKEN_TTL_S }
}

/** Signature + expiry check only — device revocation is the caller's D1 check where required. */
export async function verifyAccessToken(
  secret: string,
  token: string,
  now: number = Date.now(),
): Promise<CliTokenClaims | null> {
  const dot = token.indexOf(".")
  if (dot <= 0) return null
  const payload = token.slice(0, dot)
  const sig = token.slice(dot + 1)
  if (!payload || !sig) return null
  const expect = await hmacHex(secret, payload)
  if (!timingSafeEqual(expect, sig)) return null
  const decoded = base64UrlDecode(payload)
  if (!decoded) return null
  let claims: CliTokenClaims
  try {
    claims = JSON.parse(decoded) as CliTokenClaims
  } catch {
    return null
  }
  if (
    typeof claims.user_id !== "string" ||
    !claims.user_id ||
    typeof claims.device_id !== "string" ||
    !claims.device_id ||
    typeof claims.exp !== "number"
  ) {
    return null
  }
  if (claims.exp * 1000 <= now) return null
  return claims
}

export function newRefreshToken(): string {
  const bytes = new Uint8Array(32)
  crypto.getRandomValues(bytes)
  return `kpr_${base64Url(bytes)}`
}

export async function sha256Hex(value: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value))
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("")
}

/** No 0/1/I/L/O/U — every character survives being read aloud or retyped. */
const CODE_ALPHABET = "ABCDEFGHJKMNPQRSTVWXYZ23456789"

/** One-time pairing code, displayed `XXXX-XXXX`; only its hash is stored. */
export function newPairingCode(): string {
  const bytes = new Uint8Array(8)
  crypto.getRandomValues(bytes)
  const chars = [...bytes].map((b) => CODE_ALPHABET[b % CODE_ALPHABET.length]).join("")
  return `${chars.slice(0, 4)}-${chars.slice(4)}`
}

/** Uppercased, separators stripped — pasted codes survive formatting drift. */
export function normalizePairingCode(code: string): string {
  return code.toUpperCase().replace(/[^A-Z0-9]/g, "")
}
