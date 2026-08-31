/**
 * Agent tunnel wire protocol v1 (docs/cli.md § Wire protocol). Control frames
 * are JSON text; body bytes are binary frames `[u32 BE request id][u8 kind][chunk]`.
 * Both ends enforce the byte/path bounds — the DO here, the CLI in Rust.
 */

export const AGENT_PROTO = 1

/** Kind 0 = request body (DO → CLI), kind 1 = response body (CLI → DO). */
export const BODY_KIND_REQUEST = 0
export const BODY_KIND_RESPONSE = 1

/** Small frames keep memory flat and interleave fairly across multiplexed requests. */
export const MAX_CHUNK_BYTES = 1024 * 1024
/** Excess is refused with fault `busy` so group failover takes the next target instead of queueing. */
export const MAX_INFLIGHT = 4
/** DO-side per-request response buffer — an honest bound in lieu of credit-based flow control. */
export const RESPONSE_BUFFER_LIMIT_BYTES = 8 * 1024 * 1024
/** First `res` frame must arrive within this of `req_end` — generous for a cold local model. */
export const FIRST_RES_TIMEOUT_MS = 120_000

export const CLOSE_REPLACED = 4001
export const CLOSE_TOKEN_EXPIRED = 4003

/** Agent-reported catalog bounds mirror the custom manual-list rules (docs/cli.md § Model catalog). */
export const MAX_REPORTED_MODELS = 100
export const MAX_REPORTED_MODEL_ID_LENGTH = 128

const OPENAI_PATHS = new Set(["/chat/completions", "/models", "/audio/transcriptions"])
const ANTHROPIC_PATHS = new Set(["/v1/messages", "/v1/messages/count_tokens", "/v1/models"])

export type CliProviderFormat = "openai" | "anthropic"

export function isAllowedPath(format: CliProviderFormat, path: string): boolean {
  return (format === "openai" ? OPENAI_PATHS : ANTHROPIC_PATHS).has(path)
}

export type AgentFaultReason = "offline" | "busy" | "timeout" | "replaced" | "too_large" | "protocol"

/** CLI-reported local failures (`res_err.reason`) → the DO's fault vocabulary. */
export function faultFromResErrReason(reason: string): AgentFaultReason {
  if (reason === "connect_refused") return "offline"
  if (reason === "timeout") return "timeout"
  return "protocol"
}

export type ControlFrame =
  | { t: "hello"; proto: number; slug: string }
  | { t: "req"; id: number; method: string; path: string; headers: Record<string, string> }
  | { t: "req_end"; id: number }
  | { t: "res"; id: number; status: number; headers: Record<string, string> }
  | { t: "res_end"; id: number }
  | { t: "res_err"; id: number; reason: string }
  | { t: "models"; models: string[] }
  | { t: "cancel"; id: number }

export function parseControlFrame(text: string): ControlFrame | null {
  let parsed: unknown
  try {
    parsed = JSON.parse(text)
  } catch {
    return null
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null
  const frame = parsed as Record<string, unknown>
  if (typeof frame.t !== "string") return null
  switch (frame.t) {
    case "res":
      if (typeof frame.id !== "number" || typeof frame.status !== "number") return null
      return {
        t: "res",
        id: frame.id,
        status: frame.status,
        headers:
          frame.headers && typeof frame.headers === "object" && !Array.isArray(frame.headers)
            ? Object.fromEntries(
                Object.entries(frame.headers as Record<string, unknown>).filter(
                  (e): e is [string, string] => typeof e[1] === "string",
                ),
              )
            : {},
      }
    case "res_end":
    case "cancel":
    case "req_end":
      if (typeof frame.id !== "number") return null
      return { t: frame.t, id: frame.id }
    case "res_err":
      if (typeof frame.id !== "number" || typeof frame.reason !== "string") return null
      return { t: "res_err", id: frame.id, reason: frame.reason }
    case "models":
      if (!Array.isArray(frame.models)) return null
      return { t: "models", models: frame.models.filter((m): m is string => typeof m === "string") }
    default:
      return null
  }
}

export function encodeBinaryFrame(id: number, kind: number, chunk: Uint8Array): Uint8Array {
  const out = new Uint8Array(5 + chunk.byteLength)
  const view = new DataView(out.buffer)
  view.setUint32(0, id, false)
  out[4] = kind
  out.set(chunk, 5)
  return out
}

export function decodeBinaryFrame(data: ArrayBuffer): { id: number; kind: number; chunk: Uint8Array } | null {
  if (data.byteLength < 5) return null
  const view = new DataView(data)
  return {
    id: view.getUint32(0, false),
    kind: view.getUint8(4),
    chunk: new Uint8Array(data, 5),
  }
}

/**
 * Bounds for an agent-reported model list: ≤ 100 entries, each trimmed to
 * 1–128 chars, no whitespace (`/` allowed). An out-of-bounds report is
 * rejected whole (`null`) rather than partially applied.
 */
export function validateModelsReport(models: string[]): string[] | null {
  if (models.length > MAX_REPORTED_MODELS) return null
  const out: string[] = []
  for (const raw of models) {
    const trimmed = raw.trim()
    if (!trimmed || trimmed.length > MAX_REPORTED_MODEL_ID_LENGTH) return null
    if (/\s/.test(trimmed)) return null
    out.push(trimmed)
  }
  return out
}
