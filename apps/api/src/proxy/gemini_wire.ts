/**
 * Shared Gemini `GenerateContent` wire shapes and the pieces both antigravity
 * conversion surfaces need (`gemini_openai.ts` for `/openai/v1`,
 * `gemini_anthropic.ts` for `/anthropic`).
 *
 * Field names follow the proto-JSON camelCase spelling the CloudCode backend
 * emits; see CLIProxyAPI `internal/translator/antigravity/**` for the
 * translator behaviour this mirrors. Nothing here knows about the antigravity
 * envelope (`{model, project, request, …}`) — that lives in the adapter.
 */

export type GeminiPart = {
  text?: string
  thought?: boolean
  thoughtSignature?: string
  inlineData?: { mimeType?: string; mime_type?: string; data?: string }
  inline_data?: { mimeType?: string; mime_type?: string; data?: string }
  functionCall?: { id?: string; name?: string; args?: unknown }
  functionResponse?: { id?: string; name?: string; response?: unknown }
}

export type GeminiContent = {
  role?: "user" | "model"
  parts?: GeminiPart[]
}

export type GeminiUsageMetadata = {
  promptTokenCount?: number
  candidatesTokenCount?: number
  thoughtsTokenCount?: number
  totalTokenCount?: number
  cachedContentTokenCount?: number
}

export type GeminiResponse = {
  candidates?: Array<{
    content?: GeminiContent
    finishReason?: string
    index?: number
  }>
  usageMetadata?: GeminiUsageMetadata
  modelVersion?: string
  responseId?: string
  createTime?: string
  promptFeedback?: { blockReason?: string }
}

/** The inner `request` object of an antigravity envelope: a plain Gemini request. */
export type GeminiRequest = {
  contents: GeminiContent[]
  systemInstruction?: GeminiContent
  tools?: Array<{ functionDeclarations: unknown[] }>
  toolConfig?: { functionCallingConfig: { mode: string; allowedFunctionNames?: string[] } }
  generationConfig?: Record<string, unknown>
  sessionId?: string
}

/** Every antigravity response — stream frame or whole body — is `{response: …}`. */
export function unwrapAntigravityResponse(json: unknown): GeminiResponse | null {
  if (!json || typeof json !== "object") return null
  const inner = (json as { response?: unknown }).response
  if (inner && typeof inner === "object") return inner as GeminiResponse
  // A bare Gemini response (no envelope) is still usable — countTokens and the
  // occasional error frame come through unwrapped.
  return json as GeminiResponse
}

export function geminiParts(resp: GeminiResponse | null): GeminiPart[] {
  const parts = resp?.candidates?.[0]?.content?.parts
  return Array.isArray(parts) ? parts : []
}

/** Both spellings appear in the wild; the backend accepts and emits either. */
export function inlineDataOf(
  part: GeminiPart,
): { mimeType: string; data: string } | null {
  const raw = part.inlineData ?? part.inline_data
  if (!raw) return null
  const data = typeof raw.data === "string" ? raw.data : ""
  if (!data) return null
  const mimeType = raw.mimeType || raw.mime_type || "image/png"
  return { mimeType, data }
}

export type NormalizedGeminiUsage = {
  promptTokens: number | null
  completionTokens: number | null
  reasoningTokens: number | null
  cachedTokens: number | null
  totalTokens: number | null
}

function num(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null
}

/**
 * `candidatesTokenCount` counts only the visible answer; `thoughtsTokenCount`
 * is billed output too but is reported separately, so both surfaces add them
 * together for their own output figure and keep the thinking half visible in
 * the details field their format has for it.
 */
export function normalizeGeminiUsage(
  usage: GeminiUsageMetadata | undefined,
): NormalizedGeminiUsage | null {
  if (!usage || typeof usage !== "object") return null
  return {
    promptTokens: num(usage.promptTokenCount),
    completionTokens: num(usage.candidatesTokenCount),
    reasoningTokens: num(usage.thoughtsTokenCount),
    cachedTokens: num(usage.cachedContentTokenCount),
    totalTokens: num(usage.totalTokenCount),
  }
}

/**
 * Candidate-level terminal reasons that mean the *output* was blocked. These
 * must not read as a successful `stop` / `end_turn` — an often-empty answer a
 * client cannot tell apart from a real blank completion.
 */
const BLOCKED_FINISH_REASONS = new Set([
  "SAFETY",
  "RECITATION",
  "BLOCKLIST",
  "PROHIBITED_CONTENT",
  "SPII",
  "IMAGE_SAFETY",
])

/**
 * Gemini `finishReason` → the OpenAI token, before the tool-call override the
 * callers apply (a turn that produced a `functionCall` is `tool_calls`
 * whatever the upstream reason said).
 */
export function openaiFinishReason(finishReason: string | undefined): string {
  const reason = (finishReason || "").toUpperCase()
  if (reason === "MAX_TOKENS") return "length"
  if (BLOCKED_FINISH_REASONS.has(reason)) return "content_filter"
  return "stop"
}

/** Gemini `finishReason` → the Anthropic `stop_reason` token. */
export function anthropicStopReason(
  finishReason: string | undefined,
  sawToolCall: boolean,
): string {
  if (sawToolCall) return "tool_use"
  const reason = (finishReason || "").toUpperCase()
  if (reason === "MAX_TOKENS") return "max_tokens"
  if (BLOCKED_FINISH_REASONS.has(reason)) return "refusal"
  return "end_turn"
}

/**
 * Split an SSE body into `data:` payload strings without buffering the whole
 * stream — one bounded carry for a partial trailing line, the same shape the
 * usage sniffers use. `[DONE]` is yielded as-is so callers can end cleanly.
 */
export async function* sseDataLines(
  source: ReadableStream<Uint8Array> | ReadableStreamDefaultReader<Uint8Array>,
): AsyncGenerator<string> {
  // Callers that need to cancel the upstream fetch from a wrapper stream's
  // `cancel()` hook acquire the reader themselves and pass it in.
  const reader = "getReader" in source ? source.getReader() : source
  const decoder = new TextDecoder()
  let carry = ""
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      carry += decoder.decode(value, { stream: true })
      const lines = carry.split("\n")
      carry = lines.pop() ?? ""
      for (const line of lines) {
        if (!line.startsWith("data:")) continue
        const data = line.slice(5).trim()
        if (data) yield data
      }
    }
    const tail = carry.trim()
    if (tail.startsWith("data:")) {
      const data = tail.slice(5).trim()
      if (data) yield data
    }
  } finally {
    try {
      reader.releaseLock()
    } catch {
      /* already released */
    }
  }
}

/**
 * A JSON Schema Gemini will accept. The backend rejects the JSON Schema
 * keywords Gemini's own `Schema` proto has no field for, so they are dropped
 * rather than passed through — the same set CLIProxyAPI strips in
 * `util.CleanJSONSchemaForAntigravityTool`. `additionalProperties` is the one
 * that bites in practice: OpenAI clients set it on every strict tool.
 */
const UNSUPPORTED_SCHEMA_KEYS = new Set([
  "$schema",
  "$id",
  "$ref",
  "$defs",
  "definitions",
  "additionalProperties",
  "unevaluatedProperties",
  "patternProperties",
  "const",
  "examples",
  "default",
  "title",
  "exclusiveMinimum",
  "exclusiveMaximum",
  "allOf",
  "oneOf",
  "not",
  "if",
  "then",
  "else",
])

export function sanitizeJsonSchema(schema: unknown): unknown {
  if (Array.isArray(schema)) return schema.map(sanitizeJsonSchema)
  if (!schema || typeof schema !== "object") return schema
  const out: Record<string, unknown> = {}
  for (const [key, value] of Object.entries(schema as Record<string, unknown>)) {
    if (UNSUPPORTED_SCHEMA_KEYS.has(key)) continue
    // `properties` maps arbitrary *property names* to schemas — the names are
    // not schema keywords, so a property that happens to be called "title" or
    // "default" must survive while its schema value is still sanitized.
    if (key === "properties" && value && typeof value === "object" && !Array.isArray(value)) {
      const properties: Record<string, unknown> = {}
      for (const [name, sub] of Object.entries(value as Record<string, unknown>)) {
        properties[name] = sanitizeJsonSchema(sub)
      }
      out[key] = properties
      continue
    }
    out[key] = sanitizeJsonSchema(value)
  }
  return out
}
