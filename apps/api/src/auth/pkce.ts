/** PKCE S256 helpers for provider OAuth. */

export async function buildPkcePair(): Promise<{
  codeVerifier: string
  codeChallenge: string
}> {
  const codeVerifier = base64Url(randomBytes(48))
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(codeVerifier),
  )
  const codeChallenge = base64Url(new Uint8Array(digest))
  return { codeVerifier, codeChallenge }
}

export function buildStateToken(): string {
  return base64Url(randomBytes(24))
}

function randomBytes(n: number): Uint8Array {
  const bytes = new Uint8Array(n)
  crypto.getRandomValues(bytes)
  return bytes
}

function base64Url(bytes: Uint8Array): string {
  let s = ""
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]!)
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
}

/** Parse Anthropic-style `code#state` paste. */
export function parseCodeHashState(value: string): { code: string; state: string } {
  const cleaned = value.trim()
  const hash = cleaned.indexOf("#")
  if (hash <= 0 || hash === cleaned.length - 1) {
    throw new Error("Paste authorization as code#state from the callback page")
  }
  return {
    code: cleaned.slice(0, hash).trim(),
    state: cleaned.slice(hash + 1).trim(),
  }
}
