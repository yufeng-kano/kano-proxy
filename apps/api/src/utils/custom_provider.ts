/** Validation, masking, and limits for user-defined custom providers. */

export type CustomProviderFormat = "openai" | "anthropic"
export type CustomProviderModelsMode = "auto" | "manual"

export const MAX_CUSTOM_PROVIDERS_PER_USER = 20
export const MAX_MANUAL_MODELS = 100
export const MAX_MANUAL_MODEL_ID_LENGTH = 128

/** Names that would collide with a builtin provider or a reserved route segment. */
export const RESERVED_SLUGS = new Set([
  "claude-code",
  "codex",
  "grok",
  "openai",
  "anthropic",
  "claude",
  "gpt",
  "gemini",
  "google",
  "api",
  "admin",
  "custom",
  "models",
  "usage",
  "keys",
  "accounts",
  "kano",
  "kano-proxy",
])

// Lowercase alphanumeric + hyphens, must start and end alphanumeric, 2-32 chars total.
const SLUG_RE = /^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/

export function isCustomProviderFormat(v: unknown): v is CustomProviderFormat {
  return v === "openai" || v === "anthropic"
}

export function isModelsMode(v: unknown): v is CustomProviderModelsMode {
  return v === "auto" || v === "manual"
}

export function validateSlug(slug: string): string | null {
  if (slug.length < 2 || slug.length > 32) {
    return "slug must be 2-32 characters"
  }
  if (!SLUG_RE.test(slug)) {
    return "slug must be lowercase alphanumeric with hyphens, starting and ending with a letter or digit"
  }
  if (RESERVED_SLUGS.has(slug)) {
    return `slug "${slug}" is reserved`
  }
  return null
}

export function validateName(name: string): string | null {
  if (!name || name.length > 64) return "name must be 1-64 characters"
  return null
}

export function validateApiKey(key: string): string | null {
  if (!key || key.length > 512) return "api_key must be 1-512 characters"
  return null
}

export function validateBaseUrlLength(url: string): string | null {
  if (url.length > 300) return "base_url must be at most 300 characters"
  return null
}

export type ManualModelsValidation =
  | { ok: true; models: string[] }
  | { ok: false; error: string }

/** `undefined` (field omitted) means "no manual models" — not an error. */
export function validateManualModels(models: unknown): ManualModelsValidation {
  if (models === undefined) return { ok: true, models: [] }
  if (!Array.isArray(models)) return { ok: false, error: "manual_models must be an array" }
  if (models.length > MAX_MANUAL_MODELS) {
    return { ok: false, error: `manual_models must have at most ${MAX_MANUAL_MODELS} entries` }
  }
  const out: string[] = []
  for (const m of models) {
    if (typeof m !== "string") return { ok: false, error: "manual_models entries must be strings" }
    const trimmed = m.trim()
    if (!trimmed) return { ok: false, error: "manual_models entries must not be empty" }
    if (trimmed.length > MAX_MANUAL_MODEL_ID_LENGTH) {
      return {
        ok: false,
        error: `manual_models entries must be at most ${MAX_MANUAL_MODEL_ID_LENGTH} characters`,
      }
    }
    // "/" is allowed (upstream ids may be namespaced, e.g. "org/model"); only
    // whitespace is rejected.
    if (/\s/.test(trimmed)) {
      return { ok: false, error: "manual_models entries must not contain whitespace" }
    }
    out.push(trimmed)
  }
  return { ok: true, models: out }
}

export function parseManualModels(json: string | null): string[] {
  if (!json) return []
  try {
    const arr = JSON.parse(json) as unknown
    return Array.isArray(arr) ? arr.filter((s): s is string => typeof s === "string") : []
  } catch {
    return []
  }
}

/**
 * Non-secret display mask: first 6 + "…" + last 4 (e.g. "sk-abc…f3a2").
 * Keys shorter than 12 chars mask everything but the last 2 characters.
 */
export function maskApiKey(key: string): string {
  if (key.length >= 12) {
    return `${key.slice(0, 6)}…${key.slice(-4)}`
  }
  const visible = Math.min(2, key.length)
  const hidden = Math.max(0, key.length - visible)
  return `${"*".repeat(hidden)}${key.slice(-visible)}`
}
