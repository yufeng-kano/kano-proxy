import type { UpstreamModel } from "./types"

export const CODEX_MODELS_ENDPOINT = "https://chatgpt.com/backend-api/codex/models"
export const CODEX_CLIENT_VERSION = "0.144.1"
export const CODEX_USER_AGENT =
  "codex_cli_rs/0.144.1 (Mac OS 26.3.1; arm64) iTerm.app/3.6.9"
/** Public catalog mirrors, tried in order when the live endpoint is bot-walled. */
export const CODEX_MODEL_MIRROR_URLS = [
  "https://models.router-for.me/codex_client_models.json",
  "https://raw.githubusercontent.com/router-for-me/models/refs/heads/main/codex_client_models.json",
] as const

const CODEX_ORIGINATOR = "codex_cli_rs"
const FETCH_TIMEOUT_MS = 10_000

type CodexModelEntry = {
  slug?: unknown
  display_name?: unknown
  visibility?: unknown
}

type CodexModelsPayload = {
  models?: unknown
}

function isJsonContentType(contentType: string | null): boolean {
  if (!contentType) return false
  const mediaType = contentType.split(";", 1)[0]?.trim().toLowerCase()
  return mediaType === "application/json" || mediaType.endsWith("+json")
}

function mapModels(payload: unknown): UpstreamModel[] | null {
  if (!payload || typeof payload !== "object") return null
  const models = (payload as CodexModelsPayload).models
  if (!Array.isArray(models)) return null

  const seen = new Set<string>()
  const mapped: UpstreamModel[] = []
  for (const value of models) {
    if (!value || typeof value !== "object" || Array.isArray(value)) continue
    const entry = value as CodexModelEntry
    if (entry.visibility === "hide") continue
    if (typeof entry.slug !== "string" || entry.slug.trim() === "") continue
    if (seen.has(entry.slug)) continue
    seen.add(entry.slug)
    mapped.push({
      id: entry.slug,
      display_name: typeof entry.display_name === "string" ? entry.display_name : null,
    })
  }
  return mapped
}

function primaryHeaders(accessToken: string, accountId: string): Record<string, string> {
  const headers: Record<string, string> = {
    Accept: "application/json",
    Authorization: `Bearer ${accessToken}`,
    Originator: CODEX_ORIGINATOR,
    "User-Agent": CODEX_USER_AGENT,
  }
  if (accountId) headers["Chatgpt-Account-Id"] = accountId
  return headers
}

function failureReason(error: unknown): string {
  if (error instanceof Error && error.message) return error.message.slice(0, 200)
  return "models fetch failed"
}

export async function fetchCodexModels(
  accessToken: string,
  accountId: string,
): Promise<{ models: UpstreamModel[]; error: string | null }> {
  const primaryUrl = `${CODEX_MODELS_ENDPOINT}?client_version=${encodeURIComponent(CODEX_CLIENT_VERSION)}`
  const sources = [primaryUrl, ...CODEX_MODEL_MIRROR_URLS]
  let lastError = "models fetch failed"

  for (const [index, url] of sources.entries()) {
    try {
      const init: RequestInit = {
        method: "GET",
        signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
      }
      if (index === 0) init.headers = primaryHeaders(accessToken, accountId)

      const response = await fetch(url, init)
      if (!response.ok) {
        lastError = `models ${response.status}`
        continue
      }
      if (!isJsonContentType(response.headers.get("content-type"))) {
        lastError = "models response is not JSON"
        continue
      }

      const body = await response.text()
      let payload: unknown
      try {
        payload = JSON.parse(body)
      } catch {
        lastError = "models JSON parse failed"
        continue
      }

      const models = mapModels(payload)
      if (!models) {
        lastError = "models response has no models array"
        continue
      }
      return { models, error: null }
    } catch (error) {
      lastError = failureReason(error)
    }
  }

  return { models: [], error: lastError }
}
