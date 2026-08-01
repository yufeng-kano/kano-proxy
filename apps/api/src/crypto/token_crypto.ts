/** AES-GCM encrypt/decrypt for upstream OAuth payloads at rest. */

export async function encryptJson(
  encryptionKeyB64: string | undefined,
  value: unknown,
): Promise<string> {
  const key = await importKey(encryptionKeyB64)
  const iv = crypto.getRandomValues(new Uint8Array(12))
  const plaintext = new TextEncoder().encode(JSON.stringify(value))
  const cipher = await crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, plaintext)
  const packed = new Uint8Array(iv.length + cipher.byteLength)
  packed.set(iv, 0)
  packed.set(new Uint8Array(cipher), iv.length)
  return bytesToBase64(packed)
}

export async function decryptJson<T>(
  encryptionKeyB64: string | undefined,
  blob: string,
): Promise<T> {
  const key = await importKey(encryptionKeyB64)
  const raw = base64ToBytes(blob)
  const iv = raw.slice(0, 12)
  const data = raw.slice(12)
  const plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv }, key, data)
  return JSON.parse(new TextDecoder().decode(plain)) as T
}

async function importKey(encryptionKeyB64: string | undefined): Promise<CryptoKey> {
  if (!encryptionKeyB64) {
    throw new Error("TOKEN_ENCRYPTION_KEY is not configured")
  }
  let bytes: Uint8Array
  try {
    bytes = base64ToBytes(encryptionKeyB64)
  } catch {
    // allow raw utf-8 dev secrets padded/hashed to 32 bytes
    const dig = await crypto.subtle.digest(
      "SHA-256",
      new TextEncoder().encode(encryptionKeyB64),
    )
    bytes = new Uint8Array(dig)
  }
  if (bytes.length !== 32) {
    const dig = await crypto.subtle.digest("SHA-256", bytes)
    bytes = new Uint8Array(dig)
  }
  return crypto.subtle.importKey("raw", bytes, "AES-GCM", false, ["encrypt", "decrypt"])
}

function bytesToBase64(bytes: Uint8Array): string {
  let s = ""
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]!)
  return btoa(s)
}

function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}
