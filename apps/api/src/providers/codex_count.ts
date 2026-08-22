/**
 * Local token counting for providers with no upstream count endpoint
 * (docs/api.md § count_tokens, docs/codex-relay.md § Token counting).
 *
 * The Worker owns the Anthropic-body → text serialization; the relay's
 * `/count-tokens` only tokenizes (o200k_base). Every failure — relay
 * unconfigured, unreachable, non-200, timeout, malformed reply — degrades to
 * `null`, which the route answers as the sentinel `{input_tokens: 0}`: a
 * failed count must never surface as an error status, because that sends
 * Claude Code into a parallel max_tokens:1 probe burst against the real
 * upstream (measured 2026-08-22).
 */

import type { Env } from "../env"
import { invalidateCodexRelayToken, mintIdToken } from "./codex_relay"

/** Bounded side fetch — a hung relay must not hold a count open forever. */
const COUNT_FETCH_TIMEOUT_MS = 20_000

function pushBlockText(out: string[], block: unknown): void {
  if (typeof block === "string") {
    if (block) out.push(block)
    return
  }
  if (!block || typeof block !== "object") return
  const b = block as Record<string, unknown>
  if (typeof b.text === "string" && b.text) out.push(b.text)
  if (typeof b.thinking === "string" && b.thinking) out.push(b.thinking)
  if (b.type === "tool_use") {
    if (typeof b.name === "string" && b.name) out.push(b.name)
    if (b.input !== undefined) out.push(JSON.stringify(b.input))
  }
  if (b.type === "tool_result") {
    const content = b.content
    if (typeof content === "string") {
      if (content) out.push(content)
    } else if (Array.isArray(content)) {
      for (const c of content) pushBlockText(out, c)
    }
  }
}

/**
 * The text a count_tokens body would put in front of the model: system,
 * message content (text / thinking / tool_use / tool_result), and tool
 * declarations. Images and redacted thinking are skipped — no honest text
 * length exists for either. This is deliberately an approximation of the
 * upstream's own prompt framing (docs/codex-relay.md § Token counting).
 */
export function anthropicCountTexts(body: Record<string, unknown>): string[] {
  const out: string[] = []
  const system = body.system
  if (typeof system === "string") {
    if (system) out.push(system)
  } else if (Array.isArray(system)) {
    for (const block of system) pushBlockText(out, block)
  }
  for (const raw of Array.isArray(body.messages) ? (body.messages as unknown[]) : []) {
    const content = (raw as { content?: unknown } | null)?.content
    if (typeof content === "string") {
      if (content) out.push(content)
    } else if (Array.isArray(content)) {
      for (const block of content) pushBlockText(out, block)
    }
  }
  for (const raw of Array.isArray(body.tools) ? (body.tools as unknown[]) : []) {
    if (!raw || typeof raw !== "object") continue
    const tool = raw as Record<string, unknown>
    if (typeof tool.name === "string" && tool.name) out.push(tool.name)
    if (typeof tool.description === "string" && tool.description) out.push(tool.description)
    if (tool.input_schema !== undefined) out.push(JSON.stringify(tool.input_schema))
  }
  return out
}

/**
 * o200k_base count via the relay, or `null` when no count could be obtained.
 * One stale-token retry on a 401/403, mirroring relayFetch's guard — beyond
 * that, degrade rather than escalate.
 */
export async function relayCountTokens(
  env: Env,
  body: Record<string, unknown>,
): Promise<number | null> {
  if (!env.CODEX_RELAY_URL || !env.CODEX_RELAY_SA_KEY) return null
  try {
    const origin = new URL(env.CODEX_RELAY_URL).origin
    const payload = JSON.stringify({ texts: anthropicCountTexts(body) })
    const attempt = async (): Promise<Response> =>
      fetch(`${origin}/count-tokens`, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-serverless-authorization": `Bearer ${await mintIdToken(env)}`,
        },
        body: payload,
        signal: AbortSignal.timeout(COUNT_FETCH_TIMEOUT_MS),
      })

    let res = await attempt()
    if ((res.status === 401 || res.status === 403) && !res.headers.has("x-relay-count")) {
      invalidateCodexRelayToken(origin)
      res = await attempt()
    }
    if (!res.ok) return null
    const tokens = ((await res.json()) as { tokens?: unknown }).tokens
    if (typeof tokens !== "number" || !Number.isFinite(tokens) || tokens < 0) return null
    return Math.round(tokens)
  } catch {
    return null
  }
}
