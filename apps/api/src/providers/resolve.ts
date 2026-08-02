import { getCustomProviderBySlug, type CustomProviderRow } from "../db/custom_providers"
import type { Env } from "../env"
import { isProviderId } from "../env"
import { splitModelId } from "../utils/model"
import { createCustomAnthropicAdapter } from "./custom_anthropic"
import { createCustomOpenAIAdapter } from "./custom_openai"
import { getAdapter } from "./index"
import type { ProviderAdapter } from "./types"

export type ResolvedModel = {
  /** Builtin `ProviderId`, or the custom provider's slug. */
  provider: string
  upstreamModel: string
  raw: string
  adapter: ProviderAdapter
  isBuiltin: boolean
  /** Present only when `isBuiltin` is false. */
  customProvider?: CustomProviderRow
}

/**
 * Shared model-id resolution for `/openai/v1` and `/anthropic`: split on the
 * first "/", try the builtin `ProviderId` union first, and only fall back to
 * a per-user `custom_providers` lookup when the prefix isn't a builtin — a
 * miss either way is the same `invalid_model` case to the caller. The custom
 * lookup is scoped to `userId` by construction, so a slug never resolves
 * across users. Adapters for custom providers are built fresh per call —
 * they carry the row's `base_url`/`format` and are never cached or added to
 * the static builtin registry.
 */
export async function resolveModel(
  env: Env,
  userId: string,
  model: string,
): Promise<ResolvedModel | null> {
  const split = splitModelId(model)
  if (!split) return null

  if (isProviderId(split.prefix)) {
    return {
      provider: split.prefix,
      upstreamModel: split.upstreamModel,
      raw: split.raw,
      adapter: getAdapter(split.prefix),
      isBuiltin: true,
    }
  }

  const row = await getCustomProviderBySlug(env.DB, userId, split.prefix)
  if (!row) return null
  const adapter =
    row.format === "anthropic" ? createCustomAnthropicAdapter(row) : createCustomOpenAIAdapter(row)
  return {
    provider: row.slug,
    upstreamModel: split.upstreamModel,
    raw: split.raw,
    adapter,
    isBuiltin: false,
    customProvider: row,
  }
}
