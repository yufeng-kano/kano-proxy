import type { ProviderId } from "../env"
import { isProviderId } from "../env"

export type ParsedModel = {
  provider: ProviderId
  upstreamModel: string
  raw: string
}

export function parseModelId(model: string): ParsedModel | null {
  const raw = model.trim()
  const slash = raw.indexOf("/")
  if (slash <= 0 || slash === raw.length - 1) return null
  const provider = raw.slice(0, slash)
  const upstreamModel = raw.slice(slash + 1)
  if (!isProviderId(provider) || !upstreamModel) return null
  return { provider, upstreamModel, raw }
}

/**
 * Anthropic surface uses the same provider/model ids as OpenAI.
 * Bare upstream ids (no provider prefix) are rejected.
 */
export function parseAnthropicModel(model: string): ParsedModel | null {
  return parseModelId(model)
}
