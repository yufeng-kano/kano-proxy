/**
 * OpenAI / OpenRouter `input_audio` content parts (docs/api.md § Audio input).
 *
 * Only conversion targets need a mime: a passthrough provider forwards the
 * client's part untouched and its upstream reads `format` itself.
 */

/** `format` token → mime, limited to what the Gemini wire accepts. */
const MIME_BY_FORMAT: Record<string, string> = {
  wav: "audio/wav",
  mp3: "audio/mp3",
  mpeg: "audio/mp3",
  aac: "audio/aac",
  flac: "audio/flac",
  ogg: "audio/ogg",
  opus: "audio/ogg",
  aiff: "audio/aiff",
}

/** Human list for the `unsupported_audio_format` error — kept next to the map it describes. */
export const SUPPORTED_AUDIO_FORMATS = "wav, mp3, aac, flac, ogg, opus or aiff"

/**
 * An `input_audio` part as inline data, or null when the part is not audio
 * or carries nothing this proxy can name a mime for. A `data:` URL's own
 * mime wins over `format`: the client stated it explicitly.
 */
export function audioInline(part: unknown): { mimeType: string; data: string } | null {
  if (!part || typeof part !== "object") return null
  const p = part as { type?: string; input_audio?: { data?: unknown; format?: unknown } }
  if (p.type !== "input_audio") return null
  const raw = typeof p.input_audio?.data === "string" ? p.input_audio.data.trim() : ""
  if (!raw) return null
  const dataUrl = /^data:([^;,]+);base64,(.+)$/s.exec(raw)
  if (dataUrl) return { mimeType: dataUrl[1]!, data: dataUrl[2]! }
  const format = typeof p.input_audio?.format === "string" ? p.input_audio.format.toLowerCase() : ""
  const mimeType = MIME_BY_FORMAT[format]
  return mimeType ? { mimeType, data: raw } : null
}

/**
 * Walk an OpenAI-shaped message list once: does it carry audio at all, and
 * is every audio part in it convertible? The route needs both before it can
 * decide between dispatching, `unsupported_modality` and
 * `unsupported_audio_format` (docs/api.md § Audio input).
 */
export function scanAudioParts(messages: unknown[]): { present: boolean; convertible: boolean } {
  let present = false
  let convertible = true
  for (const message of messages) {
    const content = (message as { content?: unknown } | null)?.content
    if (!Array.isArray(content)) continue
    for (const part of content) {
      if (!part || typeof part !== "object") continue
      if ((part as { type?: string }).type !== "input_audio") continue
      present = true
      if (!audioInline(part)) convertible = false
    }
  }
  return { present, convertible }
}
