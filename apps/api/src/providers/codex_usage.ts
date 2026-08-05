/**
 * Codex subscription usage fetch.
 *
 * chatgpt.com 403s this endpoint from a Worker, but NOT by TLS fingerprint —
 * an earlier version of this comment said so and was wrong on both counts
 * (it also claimed curl is blocked; curl passes). Measured 2026-08-03 from a
 * residential IP with plain curl and a fake token: the request succeeds (401
 * JSON) until `CF-Worker` or `CF-Connecting-IP` is present, at which point it
 * is 403 HTML — any value, empty included. Cloudflare adds both to every
 * outbound fetch(), so no header tuning here can avoid it, and the same rule
 * blocks /codex/responses too (chat does NOT still work — the old comment's
 * last line was wrong as well). Full evidence table in docs/providers.md.
 */

import type { CodexUpstream } from "./codex_relay"
import { relayFetch } from "./codex_relay"

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
  upstream?: CodexUpstream,
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

  const base = upstream?.base ?? CODEX_BASE
  const paths = ["/wham/usage", "/codex/usage"]
  let lastStatus = 0
  let lastBody = ""
  let sawHtmlChallenge = false

  for (const path of paths) {
    try {
      const init: RequestInit = {
        method: "GET",
        headers: codexUsageHeaders(accessToken, accountId),
        redirect: "follow",
      }
      const res = upstream
        ? await relayFetch(upstream, `${base}${path}`, init)
        : await fetch(`${base}${path}`, init)
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

      // HTML challenge / cloudflare / bot wall. A challenged alias must not
      // prevent the remaining alias from being tried.
      const looksHtml = /^\s*</.test(text) || /just a moment|cf-browser|challenge/i.test(text)
      if (res.status === 403 && looksHtml) sawHtmlChallenge = true
      // Real auth failure or a challenged path: try the next alias first; if
      // all fail, classify below.
      if (res.status === 401 || res.status === 403) continue
    } catch (e) {
      lastBody = e instanceof Error ? e.message : "fetch error"
    }
  }

  const edgeBlocked = lastStatus === 403 || sawHtmlChallenge
  return {
    ok: false,
    status: lastStatus,
    payload: null,
    edgeBlocked,
    error: edgeBlocked
      ? sawHtmlChallenge
        ? "usage edge blocked (403 bot challenge)"
        : "usage edge blocked (403)"
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
