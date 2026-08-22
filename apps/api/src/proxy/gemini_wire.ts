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
 * The fields Gemini's `Schema` actually has, read from the Antigravity CLI's
 * embedded descriptor for `google.cloud.aiplatform.master.Schema` (extracted
 * 2026-08-22). This is an **allowlist**, and deliberately so: the backend
 * rejects the whole request with a `400` naming any field its proto lacks, so
 * a denylist fails open — every JSON Schema keyword nobody thought of is an
 * outage. `propertyNames` was exactly that (Claude Code sends it; every tool
 * call 400'd until it was added).
 *
 * Fields the proto *does* have but that are annotated `GOOGLE_INTERNAL` — and
 * are rejected for external callers — are omitted here on purpose:
 * `prefix_items`, `one_of`, `all_of`, `additional_properties_schema`. So are
 * `additionalProperties`, `title`, `default` and `defs`, which the backend
 * rejects in practice (the first is the one that bites: OpenAI clients set it
 * on every strict tool).
 */
const SUPPORTED_SCHEMA_KEYS = new Set([
  "type",
  "format",
  "description",
  "nullable",
  "enum",
  "items",
  "minItems",
  "maxItems",
  "properties",
  "propertyOrdering",
  "required",
  "minProperties",
  "maxProperties",
  "minimum",
  "maximum",
  "minLength",
  "maxLength",
  "pattern",
  "example",
  "anyOf",
])

/** `{"type": "null"}` and nothing else — JSON Schema's way of saying nullable. */
function isNullSchema(v: unknown): boolean {
  if (!v || typeof v !== "object" || Array.isArray(v)) return false
  const keys = Object.keys(v as Record<string, unknown>)
  return keys.length === 1 && (v as Record<string, unknown>).type === "null"
}

/**
 * JSON Schema spells "nullable" two ways Gemini's `Schema` cannot hold, because
 * its `type` is an enum with no NULL member: `anyOf: [X, {"type": "null"}]` and
 * `type: ["string", "null"]`. Forwarding either reaches Claude-behind-Antigravity
 * as a schema its own validator rejects (`tools.N.custom.input_schema: JSON
 * schema is invalid`, measured 2026-08-22), so both are folded into the
 * `nullable: true` field Gemini does have.
 */
function foldNullable(out: Record<string, unknown>): Record<string, unknown> {
  if (Array.isArray(out.type)) {
    const types = (out.type as unknown[]).filter((t) => t !== "null")
    if (types.length < (out.type as unknown[]).length) out.nullable = true
    // More than one non-null type has no Gemini representation either; keeping
    // the first is closer than emitting an array the backend will reject.
    if (types.length) out.type = types[0]
    else delete out.type
  }
  if (Array.isArray(out.anyOf)) {
    const members = (out.anyOf as unknown[]).filter((m) => !isNullSchema(m))
    if (members.length < (out.anyOf as unknown[]).length) out.nullable = true
    if (members.length === 1 && members[0] && typeof members[0] === "object") {
      // A single remaining branch is the schema itself; anyOf around one member
      // is noise, and Gemini rejects an anyOf that also carries sibling fields.
      const { anyOf: _drop, ...rest } = out
      return { ...(members[0] as Record<string, unknown>), ...rest }
    }
    if (members.length) out.anyOf = members
    else delete out.anyOf
  }
  return out
}

/**
 * Per-model-family schema limits. Measured against Antigravity 2026-08-22 with
 * a two-branch `anyOf` on a tool parameter: a **Gemini** model accepts it, a
 * **Claude** model behind the same endpoint answers `tools.N.custom
 * .input_schema: JSON schema is invalid` from its own Vertex-side validator.
 * Google's Gemini→Claude translation evidently cannot carry the union, so on
 * that family the union is dropped and the parameter is left unconstrained —
 * a typeless property is accepted by both, and the description survives to
 * guide the model. Same shape of rule as VALIDATED function calling
 * (docs/providers.md § Antigravity).
 */
export type SchemaDialect = { allowAnyOf: boolean }

export const GEMINI_SCHEMA_DIALECT: SchemaDialect = { allowAnyOf: true }
export const CLAUDE_SCHEMA_DIALECT: SchemaDialect = { allowAnyOf: false }

/** `claude` in the upstream model id — the same test the request builder uses. */
export function schemaDialectFor(model: string): SchemaDialect {
  return model.toLowerCase().includes("claude") ? CLAUDE_SCHEMA_DIALECT : GEMINI_SCHEMA_DIALECT
}

export function sanitizeJsonSchema(
  schema: unknown,
  dialect: SchemaDialect = GEMINI_SCHEMA_DIALECT,
): unknown {
  if (Array.isArray(schema)) return schema.map((v) => sanitizeJsonSchema(v, dialect))
  if (!schema || typeof schema !== "object") return schema
  const out: Record<string, unknown> = {}
  for (const [key, value] of Object.entries(schema as Record<string, unknown>)) {
    if (!SUPPORTED_SCHEMA_KEYS.has(key)) continue
    // `properties` maps arbitrary *property names* to schemas — the names are
    // not schema keywords, so a property that happens to be called "title" or
    // "default" must survive while its schema value is still sanitized.
    if (key === "properties" && value && typeof value === "object" && !Array.isArray(value)) {
      const properties: Record<string, unknown> = {}
      for (const [name, sub] of Object.entries(value as Record<string, unknown>)) {
        properties[name] = sanitizeJsonSchema(sub, dialect)
      }
      out[key] = properties
      continue
    }
    out[key] = sanitizeJsonSchema(value, dialect)
  }
  const folded = foldNullable(out)
  // A surviving multi-branch anyOf is a genuine union; foldNullable already
  // collapsed the `[X, null]` spelling into a single schema.
  if (!dialect.allowAnyOf && Array.isArray(folded.anyOf)) delete folded.anyOf
  return folded
}
