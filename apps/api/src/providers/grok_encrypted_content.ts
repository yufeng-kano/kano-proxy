/**
 * Transport-shape check for xAI/Grok reasoning.encrypted_content.
 *
 * Does not prove decryptability. Rejects known Claude/GPT/Gemini envelopes and
 * low-entropy blobs so foreign Anthropic thinking.signature values are never
 * forwarded as Grok ciphertext. Inspired by CLIProxyAPI InspectGrokEncryptedContent.
 */

const MAX_LEN = 8 * 1024 * 1024
const MIN_DECODED_LEN = 32
const MIN_ENTROPY_RATIO = 0.85

const BASE64_STD = (() => {
  const set = new Uint8Array(128)
  const alphabet =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
  for (let i = 0; i < alphabet.length; i++) set[alphabet.charCodeAt(i)] = 1
  return set
})()

/** First chars that can begin a self-describing foreign envelope. */
const SELF_DESCRIBING_FIRST =
  "CERg" /* Claude E/R/CAIS + GPT gAAAA — enough for fast reject */

export function isValidGrokEncryptedContent(raw: string): boolean {
  return inspectGrokEncryptedContent(raw) === null
}

/** Returns an error reason, or null when the blob looks replay-safe for xAI. */
export function inspectGrokEncryptedContent(raw: string): string | null {
  if (typeof raw !== "string" || !raw) return "empty"
  if (raw !== raw.trim()) return "whitespace"
  if (raw.length > MAX_LEN) return "too_long"
  // Provider / GPT envelope rejects before the base64 alphabet scan — foreign
  // signatures often contain `#` or `-` that would otherwise report non_base64.
  const hash = raw.indexOf("#")
  if (hash > 0 && hash < 32 && /^[a-z0-9_-]+$/i.test(raw.slice(0, hash))) {
    return "provider_prefix"
  }
  if (raw.startsWith("gAAAA")) return "gpt_envelope"
  if (raw.includes("=")) return "padded_base64"
  for (let i = 0; i < raw.length; i++) {
    const c = raw.charCodeAt(i)
    if (c >= 128 || !BASE64_STD[c]) return "non_base64"
  }
  if (SELF_DESCRIBING_FIRST.includes(raw[0]!)) {
    if (looksLikeClaudeThinkingSignature(raw)) return "claude_thinking"
    if (looksLikeClaudeCaisSignature(raw)) return "claude_cais"
  }

  let decoded: Uint8Array
  try {
    decoded = decodeRawStdBase64(raw)
  } catch {
    return "base64_decode"
  }
  if (decoded.length < MIN_DECODED_LEN) return "too_short"
  if (byteEntropyRatio(decoded) < MIN_ENTROPY_RATIO) return "low_entropy"
  return null
}

/**
 * Classic Claude thinking envelope (CPA Strict spirit): after base64 decode the
 * payload must start with protobuf magic 0x12. An E/R first character alone is
 * NOT enough — ~1/64 of uniform Grok ciphertext is E-prefixed and must still
 * be accepted.
 */
function looksLikeClaudeThinkingSignature(sig: string): boolean {
  if (sig[0] !== "E" && sig[0] !== "R") return false
  try {
    if (sig[0] === "E") {
      return claudeSingleLayerMagicOk(sig)
    }
    // R-form: outer decode is ASCII E-form, which must itself carry 0x12 magic.
    const outer = decodeRawStdBase64(sig)
    if (outer.length === 0 || outer[0] !== 0x45 /* E */) return false
    let inner = ""
    for (let i = 0; i < outer.length; i++) inner += String.fromCharCode(outer[i]!)
    // Inner E-form may still carry standard padding characters.
    if (inner[0] !== "E") return false
    return claudeSingleLayerMagicOk(inner.replace(/=+$/, ""))
  } catch {
    return false
  }
}

function claudeSingleLayerMagicOk(eForm: string): boolean {
  if (eForm[0] !== "E") return false
  const decoded = decodeRawStdBase64(eForm)
  // Magic byte 0x12 identifies the classic Claude thinking protobuf envelope.
  return decoded.length > 0 && decoded[0] === 0x12
}

function looksLikeClaudeCaisSignature(sig: string): boolean {
  // CAIS envelopes start with 'C'; decoded first byte 0x08; model text has "claude-".
  if (sig[0] !== "C") return false
  try {
    const decoded = decodeRawStdBase64(sig)
    if (decoded.length < 8 || decoded[0] !== 0x08) return false
    const asText = new TextDecoder().decode(decoded)
    return asText.includes("claude-")
  } catch {
    return false
  }
}

function decodeRawStdBase64(sig: string): Uint8Array {
  // atob requires padding; restore it for decode only.
  const pad = sig.length % 4 === 0 ? "" : "=".repeat(4 - (sig.length % 4))
  const bin = atob(sig + pad)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

function byteEntropyRatio(buf: Uint8Array): number {
  if (buf.length === 0) return 0
  const counts = new Array<number>(256).fill(0)
  for (let i = 0; i < buf.length; i++) counts[buf[i]!]!++
  const n = buf.length
  let entropy = 0
  for (let i = 0; i < 256; i++) {
    const c = counts[i]!
    if (!c) continue
    const p = c / n
    entropy -= p * Math.log2(p)
  }
  const maxSymbols = Math.min(n, 256)
  if (maxSymbols <= 1) return 0
  return entropy / Math.log2(maxSymbols)
}
