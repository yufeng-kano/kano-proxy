/** API key generation and hashing (no secrets in logs). */

const KEY_PREFIX = "sk-kano-proxy-"
/** Constant prefix (14 chars) + 6 distinguishing chars, so two keys are visually distinct. */
const DISPLAY_PREFIX_LENGTH = 20

export async function hashApiKey(plaintext: string): Promise<string> {
  const data = new TextEncoder().encode(plaintext)
  const digest = await crypto.subtle.digest("SHA-256", data)
  return hex(new Uint8Array(digest))
}

export async function createApiKeyMaterial(): Promise<{
  plaintext: string
  prefix: string
  hash: string
}> {
  const bytes = new Uint8Array(24)
  crypto.getRandomValues(bytes)
  const body = base64Url(bytes)
  const plaintext = `${KEY_PREFIX}${body}`
  const hash = await hashApiKey(plaintext)
  return { plaintext, prefix: plaintext.slice(0, DISPLAY_PREFIX_LENGTH), hash }
}

export function extractBearer(authHeader: string | undefined): string | null {
  if (!authHeader) return null
  const m = authHeader.match(/^Bearer\s+(.+)$/i)
  return m ? m[1]!.trim() : null
}

function base64Url(bytes: Uint8Array): string {
  let s = ""
  for (const b of bytes) s += String.fromCharCode(b)
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
}

function hex(bytes: Uint8Array): string {
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("")
}
