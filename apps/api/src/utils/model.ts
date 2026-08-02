import type { ProviderId } from "../env"
import { isProviderId } from "../env"

export type ParsedModel = {
  provider: ProviderId
  upstreamModel: string
  raw: string
}

export type SplitModelId = {
  /** Text before the first "/" — a builtin provider id or a candidate custom slug. */
  prefix: string
  upstreamModel: string
  raw: string
}

/**
 * Split on the FIRST "/" only — upstream ids may legitimately contain
 * further slashes (e.g. a custom slug fronting `org/model`), so the rest of
 * the string after the first separator is the upstream id verbatim.
 */
export function splitModelId(model: string): SplitModelId | null {
  const raw = model.trim()
  const slash = raw.indexOf("/")
  if (slash <= 0 || slash === raw.length - 1) return null
  const prefix = raw.slice(0, slash)
  const upstreamModel = raw.slice(slash + 1)
  if (!upstreamModel) return null
  return { prefix, upstreamModel, raw }
}

export function parseModelId(model: string): ParsedModel | null {
  const split = splitModelId(model)
  if (!split || !isProviderId(split.prefix)) return null
  return { provider: split.prefix, upstreamModel: split.upstreamModel, raw: split.raw }
}

/**
 * Anthropic surface uses the same provider/model ids as OpenAI.
 * Bare upstream ids (no provider prefix) are rejected.
 */
export function parseAnthropicModel(model: string): ParsedModel | null {
  return parseModelId(model)
}
