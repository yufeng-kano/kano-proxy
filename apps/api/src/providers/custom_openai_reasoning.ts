import {
  nearestReasoningEffort,
  parseReasoningEffort,
  type ReasoningEffort,
} from "../utils/reasoning"

export type EffortRejection = {
  rejected: ReasoningEffort
  allowed: ReasoningEffort[]
}

export type EffortRejectionParser = (bodyText: string) => EffortRejection | null

/**
 * Tabby + Qwen 3.8 Jinja:
 * `TemplateError: Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low.`
 * Matches anywhere so FastAPI `{"detail":"…"}` and a bare exception string both work.
 */
export const parseTabbyQwenTemplateError: EffortRejectionParser = (bodyText) => {
  const m = bodyText.match(
    /unexpected reasoning effort\s+(\S+?)\.\s*supported types are\s+(.+)/i,
  )
  if (!m) return null
  const rejected = parseEffortToken(m[1])
  if (!rejected) return null
  const allowed = parseSupportedEffortList(m[2])
  if (allowed.length === 0) return null
  return { rejected, allowed }
}

const PARSERS: EffortRejectionParser[] = [parseTabbyQwenTemplateError]

export function parseUnsupportedEffortRejection(bodyText: string): EffortRejection | null {
  for (const parse of PARSERS) {
    const result = parse(bodyText)
    if (result) return result
  }
  return null
}

/** Rewrite only `reasoning_effort` when a parser + nearest-neighbor say to retry. */
export function remapUnsupportedEffortBody(
  sentBody: Record<string, unknown>,
  bodyText: string,
): Record<string, unknown> | null {
  const sent = parseReasoningEffort(sentBody.reasoning_effort)
  if (sent === undefined || sent === "invalid") return null

  const parsed = parseUnsupportedEffortRejection(bodyText)
  if (!parsed) return null
  if (parsed.allowed.includes(parsed.rejected)) return null

  const mapped = nearestReasoningEffort(parsed.rejected, parsed.allowed)
  if (!mapped || mapped === sent) return null

  return { ...sentBody, reasoning_effort: mapped }
}

function parseSupportedEffortList(raw: string): ReasoningEffort[] {
  const cleaned = raw.replace(/\(\s*default\s*\)/gi, " ")
  const allowed: ReasoningEffort[] = []
  for (const part of cleaned.split(/,|\band\b/i)) {
    const token = parseEffortToken(part)
    if (token) allowed.push(token)
  }
  return allowed
}

function parseEffortToken(raw: string): ReasoningEffort | null {
  const trimmed = raw.trim().replace(/^[^a-z0-9]+|[^a-z0-9]+$/gi, "")
  const parsed = parseReasoningEffort(trimmed)
  return parsed && parsed !== "invalid" ? parsed : null
}
