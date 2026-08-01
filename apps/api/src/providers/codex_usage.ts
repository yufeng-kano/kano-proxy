/**
 * Codex subscription usage fetch.
 *
 * chatgpt.com edge bot-challenges /codex/usage by TLS/client fingerprint,
 * not headers. Verified 2026-08-01 with identical headers + IP + fake token:
 * stdlib urllib → 401 JSON (passes the wall), curl and workerd fetch() →
 * 403 HTML challenge. lincy passes only because Python urllib's fingerprint
 * is allowed; a Worker cannot change its fetch() fingerprint, so header
 * tuning cannot fix this — expect edgeBlocked from Workers until the edge
 * rules change. Failure is non-fatal for the account pool (chat still works;
 * /codex/responses is not bot-walled).
 *
 * @see lincy-agent docs/dev/provider-api-spec.md Codex usage endpoint
 * @see lincy-agent src/codex_proxy/service.py _sync_usage_get
 */

export const CODEX_CLI_UA = "codex_cli_rs/0.144.3"
const CODEX_BASE = "https://chatgpt.com/backend-api"

export type CodexUsagePayload = {
  email?: string
  plan_type?: string
  user_id?: string
  account_id?: string
  rate_limit?: {
    allowed?: boolean
    limit_reached?: boolean
    primary_window?: CodexRateWindow | null
    secondary_window?: CodexRateWindow | null
  }
}

export type CodexRateWindow = {
  used_percent?: number
  limit_window_seconds?: number
  reset_after_seconds?: number
  reset_at?: number
}

export type CodexUsageResult = {
  ok: boolean
  status: number
  payload: CodexUsagePayload | null
  /** true when edge bot-challenged or transient; account still usable for chat */
  edgeBlocked: boolean
  error: string | null
}

export function codexUsageHeaders(accessToken: string, accountId: string): Record<string, string> {
  return {
    Authorization: `Bearer ${accessToken}`,
    "chatgpt-account-id": accountId,
    "OpenAI-Beta": "responses=experimental",
    originator: "codex_cli_rs",
    Accept: "application/json",
    "User-Agent": CODEX_CLI_UA,
    "Accept-Language": "en-US,en;q=0.9",
  }
}

export async function fetchCodexUsageJson(
  accessToken: string,
  accountId: string,
): Promise<CodexUsageResult> {
  if (!accountId) {
    return {
      ok: false,
      status: 0,
      payload: null,
      edgeBlocked: false,
      error: "missing chatgpt account id",
    }
  }

  const paths = ["/codex/usage", "/wham/usage"]
  let lastStatus = 0
  let lastBody = ""

  for (const path of paths) {
    try {
      const res = await fetch(`${CODEX_BASE}${path}`, {
        method: "GET",
        headers: codexUsageHeaders(accessToken, accountId),
        redirect: "follow",
      })
      lastStatus = res.status
      const text = await res.text()
      lastBody = text.slice(0, 200)

      if (res.ok) {
        try {
          const payload = JSON.parse(text) as CodexUsagePayload
          return { ok: true, status: res.status, payload, edgeBlocked: false, error: null }
        } catch {
          return {
            ok: false,
            status: res.status,
            payload: null,
            edgeBlocked: false,
            error: "usage JSON parse failed",
          }
        }
      }

      // HTML challenge / cloudflare / bot wall
      const looksHtml = /^\s*</.test(text) || /just a moment|cf-browser|challenge/i.test(text)
      if (res.status === 403 && looksHtml) {
        return {
          ok: false,
          status: 403,
          payload: null,
          edgeBlocked: true,
          error: "usage edge blocked (403 bot challenge)",
        }
      }
      // real auth failure
      if (res.status === 401 || res.status === 403) {
        // try next path first; if all fail, classify below
        continue
      }
    } catch (e) {
      lastBody = e instanceof Error ? e.message : "fetch error"
    }
  }

  const edgeBlocked = lastStatus === 403
  return {
    ok: false,
    status: lastStatus,
    payload: null,
    edgeBlocked,
    error: edgeBlocked
      ? "usage edge blocked (403)"
      : `usage ${lastStatus || "error"}${lastBody ? `: ${lastBody}` : ""}`,
  }
}

export function windowsFromCodexPayload(payload: CodexUsagePayload): Array<{
  label: string
  utilization: number | null
  resets_at: string | null
}> {
  const windows: Array<{
    label: string
    utilization: number | null
    resets_at: string | null
  }> = []
  for (const w of [
    payload.rate_limit?.primary_window,
    payload.rate_limit?.secondary_window,
  ]) {
    if (!w) continue
    windows.push({
      label: windowLabel(w.limit_window_seconds),
      utilization: w.used_percent ?? null,
      resets_at: w.reset_at ? new Date(w.reset_at * 1000).toISOString() : null,
    })
  }
  return windows
}

function windowLabel(seconds: number | undefined): string {
  if (!seconds) return "window"
  if (seconds === 604800) return "Week"
  if (seconds % 3600 === 0 && seconds < 86400) return `${seconds / 3600}h`
  if (seconds % 86400 === 0) return `${seconds / 86400}d`
  return `${seconds}s`
}
