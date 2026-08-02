import { isValidGrokEncryptedContent } from "../../src/providers/grok_encrypted_content"

/** Deterministic high-entropy unpadded standard base64 for unit tests. */
export function fakeGrokEncryptedContent(seed = 1): string {
  for (let attempt = 0; attempt < 32; attempt++) {
    const bytes = new Uint8Array(48)
    for (let i = 0; i < bytes.length; i++) {
      bytes[i] =
        (Math.imul(seed + attempt + i, 1103515245) + 12345 + i * 17) >>> 0 & 0xff
    }
    bytes[0] = 0x7f
    bytes[1] = 0xa3
    bytes[2] = 0x11
    let bin = ""
    for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]!)
    const b64 = btoa(bin).replace(/=+$/, "")
    if (isValidGrokEncryptedContent(b64)) return b64
  }
  throw new Error("failed to synthesize valid grok encrypted_content fixture")
}
