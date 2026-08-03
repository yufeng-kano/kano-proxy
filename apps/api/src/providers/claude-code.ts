import type { Env } from "../env"
import type { AcquiredAccount } from "../pool/acquire"
import { saveCredential } from "../pool/acquire"
import { mapReasoning } from "../utils/reasoning"
import type { ChatCompletionRequest, ProviderAdapter, UsageWindow } from "./types"
import { openaiToAnthropicMessages, anthropicToOpenAIResponse } from "../proxy/openai_anthropic"

const ANTHROPIC_API = "https://api.anthropic.com"
const OAUTH_TOKEN = "https://console.anthropic.com/v1/oauth/token"
const REQUIRED_SYSTEM =
  "You are Claude Code, Anthropic's official CLI for Claude."

const DEFAULT_CLIENT_ID = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"

/**
 * Claude Code CLI client fingerprint. The OAuth upstream expects requests to
 * look like the CLI that owns these tokens; sending workerd's default
 * `User-Agent` (or none) is the same bot-wall exposure that already bites the
 * codex path. A real client's own values are forwarded when it sends them —
 * these are only the fallback for surfaces with no client headers to relay.
 */
export const CLAUDE_CLIENT_FINGERPRINT: Record<string, string> = {
  "user-agent": "claude-cli/2.1.63 (external, cli)",
  "x-stainless-package-version": "0.74.0",
  "x-stainless-runtime-version": "v24.3.0",
  "x-stainless-os": "MacOS",
  "x-stainless-arch": "arm64",
}

/**
 * The fingerprint, preferring whatever the client already sent for each field
 * so a genuine Claude Code client keeps its own identity end to end.
 */
function clientFingerprint(headers?: Headers): Record<string, string> {
  const out: Record<string, string> = {}
  for (const [name, fallback] of Object.entries(CLAUDE_CLIENT_FINGERPRINT)) {
    out[name] = headers?.get(name)?.trim() || fallback
  }
  return out
}

function clientId(env: Env): string {
  return env.CLAUDE_CODE_OAUTH_CLIENT_ID || DEFAULT_CLIENT_ID
}

async function refreshClaude(
  env: Env,
  account: AcquiredAccount,
): Promise<AcquiredAccount> {
  const { credential } = account
  if (!credential.refresh_token) return account
  const exp = credential.expires_at ? Date.parse(credential.expires_at) : 0
  if (exp && exp - 60_000 > Date.now()) return account

  const res = await fetch(OAUTH_TOKEN, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      grant_type: "refresh_token",
      refresh_token: credential.refresh_token,
      client_id: credential.client_id || clientId(env),
    }),
  })
  if (!res.ok) return account
  const json = (await res.json()) as {
    access_token: string
    refresh_token?: string
    expires_in?: number
  }
  const next = {
    ...credential,
    access_token: json.access_token,
    refresh_token: json.refresh_token ?? credential.refresh_token,
    expires_at: json.expires_in
      ? new Date(Date.now() + json.expires_in * 1000).toISOString()
      : credential.expires_at,
  }
  await saveCredential(env, account.row.id, next)
  return { row: account.row, credential: next }
}

const EFFORT_BETA = "effort-2025-11-24"

// The only two betas the Claude Code OAuth upstream requires to accept a
// request at all. Always first, in this order, on every native-passthrough
// and conversion-path request.
const REQUIRED_BETAS = ["oauth-2025-04-20", "claude-code-20250219"]

// Feature betas the `/openai/v1` conversion path opts into unconditionally,
// since there the proxy authors the upstream Anthropic request itself and
// has no client beta header to be faithful to. NOT used by the native
// passthrough — see `resolveBetaHeader`.
const CONVERSION_BETAS = [
  "interleaved-thinking-2025-05-14",
  "fine-grained-tool-streaming-2025-05-14",
]

/**
 * Builds an `anthropic-beta` header value: `REQUIRED_BETAS` first, then each
 * comma-separated entry of `extra` in order, deduped against the required
 * pair and against earlier entries in `extra` itself.
 */
export function betaHeaders(extra?: string | null): string {
  const base = [...REQUIRED_BETAS]
  if (extra) {
    for (const p of extra.split(",")) {
      const t = p.trim()
      if (t && !base.includes(t)) base.push(t)
    }
  }
  return base.join(",")
}

/**
 * anthropic-beta header for the native /anthropic passthrough — faithful,
 * not opinionated: the two OAuth-required betas, then the client's own
 * `anthropic-beta` list verbatim (deduped, client order preserved). Feature
 * betas such as `interleaved-thinking-2025-05-14` /
 * `fine-grained-tool-streaming-2025-05-14` are never force-added here —
 * whether they're on is the client's choice, so proxied model behavior
 * matches a direct Anthropic connection. The one exception:
 * `effort-2025-11-24` is still added automatically when the (patched) body
 * carries `output_config`, since Anthropic needs that beta to honor
 * `output_config.effort` — deduped the same way client-supplied extras are,
 * so a client that already sends it is not doubled.
 */
export function resolveBetaHeader(opts: {
  clientBeta?: string | null
  hasOutputConfig: boolean
}): string {
  const extras = [opts.clientBeta, opts.hasOutputConfig ? EFFORT_BETA : null]
    .filter((v): v is string => !!v)
    .join(",")
  return betaHeaders(extras || null)
}

function prependRequiredSystem(body: Record<string, unknown>): Record<string, unknown> {
  const sys = body.system
  if (typeof sys === "string") {
    if (sys.startsWith(REQUIRED_SYSTEM)) return body
    return {
      ...body,
      system: [
        { type: "text", text: REQUIRED_SYSTEM },
        { type: "text", text: sys },
      ],
    }
  }
  if (Array.isArray(sys)) {
    const first = sys[0] as { type?: string; text?: string } | undefined
    if (first?.type === "text" && first.text === REQUIRED_SYSTEM) return body
    return {
      ...body,
      system: [{ type: "text", text: REQUIRED_SYSTEM }, ...sys],
    }
  }
  return {
    ...body,
    system: [{ type: "text", text: REQUIRED_SYSTEM }],
  }
}

/**
 * Shared native-passthrough forward for `/v1/messages` and
 * `/v1/messages/count_tokens` — same auth injection, beta header resolution,
 * and required-system prepend either endpoint needs.
 */
async function forwardToAnthropic(
  url: string,
  env: Env,
  account: AcquiredAccount,
  body: unknown,
  headers: Headers,
): Promise<Response> {
  const acc = await refreshClaude(env, account)
  const raw = typeof body === "object" && body ? { ...(body as object) } : {}
  const patched = prependRequiredSystem(raw as Record<string, unknown>)
  const clientBeta = headers.get("anthropic-beta")
  const hasOutputConfig = "output_config" in patched
  return fetch(url, {
    method: "POST",
    headers: {
      authorization: `Bearer ${acc.credential.access_token}`,
      "content-type": "application/json",
      "anthropic-version": headers.get("anthropic-version") || "2023-06-01",
      "anthropic-beta": resolveBetaHeader({ clientBeta, hasOutputConfig }),
      ...clientFingerprint(headers),
    },
    body: JSON.stringify(patched),
  })
}

export const claudeCodeAdapter: ProviderAdapter = {
  id: "claude-code",

  async refreshIfNeeded(env, account) {
    return refreshClaude(env, account)
  },

  async listModels(env, account) {
    const acc = await refreshClaude(env, account)
    try {
      const res = await fetch(`${ANTHROPIC_API}/v1/models?limit=100`, {
        headers: {
          authorization: `Bearer ${acc.credential.access_token}`,
          "anthropic-version": "2023-06-01",
          "anthropic-beta": "oauth-2025-04-20",
          ...clientFingerprint(),
        },
      })
      if (!res.ok) {
        return { models: [], error: `models ${res.status}` }
      }
      const json = (await res.json()) as {
        data?: Array<{ id?: string; display_name?: string }>
      }
      const models = (json.data ?? [])
        .filter((m) => typeof m.id === "string" && m.id)
        .map((m) => ({
          id: m.id as string,
          display_name: m.display_name ?? null,
        }))
      return { models, error: null }
    } catch (e) {
      return {
        models: [],
        error: e instanceof Error ? e.message : "models fetch failed",
      }
    }
  },

  async messages(env, account, body, headers) {
    return forwardToAnthropic(`${ANTHROPIC_API}/v1/messages`, env, account, body, headers)
  },

  async countTokens(env, account, body, headers) {
    return forwardToAnthropic(
      `${ANTHROPIC_API}/v1/messages/count_tokens`,
      env,
      account,
      body,
      headers,
    )
  },

  async chatCompletions(env, account, req) {
    const acc = await refreshClaude(env, account)
    const mapped = mapReasoning("claude-code", req.reasoning_effort)
    const anthropicBody = openaiToAnthropicMessages({
      model: req.upstreamModel,
      messages: req.messages,
      max_tokens: req.max_tokens ?? 4096,
      stream: req.stream,
      tools: req.tools,
      tool_choice: req.tool_choice,
      response_format: req.response_format,
      thinking: mapped.thinking,
      output_config: mapped.output_config,
      stop: req.stop,
      temperature: req.temperature,
      top_p: req.top_p,
    })
    // OpenAI→Claude: do NOT add cache_control
    const withSystem = prependRequiredSystem(anthropicBody)
    const res = await fetch(`${ANTHROPIC_API}/v1/messages`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${acc.credential.access_token}`,
        "content-type": "application/json",
        "anthropic-version": "2023-06-01",
        "anthropic-beta": betaHeaders([...CONVERSION_BETAS, EFFORT_BETA].join(",")),
        // No client headers to relay on this surface — the proxy authors the
        // whole upstream request, so the fallback fingerprint is all there is.
        ...clientFingerprint(),
      },
      body: JSON.stringify(withSystem),
    })

    if (req.stream) {
      // Pass through Anthropic SSE; clients on OpenAI path may need conversion.
      // Convert stream is complex; for MVP non-stream convert, stream passthrough as anthropic events
      // wrapped is imperfect — convert non-stream fully; stream: relay raw with note.
      if (!res.ok || !res.body) return res
      // Same model id as the non-stream path: client-facing provider/model
      return anthropicStreamToOpenAI(res, req.rawModel)
    }

    const text = await res.text()
    if (!res.ok) {
      return new Response(text, {
        status: res.status,
        headers: { "content-type": res.headers.get("content-type") || "application/json" },
      })
    }
    try {
      const json = JSON.parse(text)
      const openai = anthropicToOpenAIResponse(json, req.rawModel)
      return Response.json(openai)
    } catch {
      return new Response(text, { status: res.status })
    }
  },

  async fetchUsage(env, account) {
    const acc = await refreshClaude(env, account)
    const headers = {
      authorization: `Bearer ${acc.credential.access_token}`,
      "anthropic-beta": "oauth-2025-04-20",
      "anthropic-version": "2023-06-01",
      ...clientFingerprint(),
    }
    const [usageRes, profileRes] = await Promise.all([
      fetch(`${ANTHROPIC_API}/api/oauth/usage`, { headers }),
      fetch(`${ANTHROPIC_API}/api/oauth/profile`, { headers }),
    ])
    if (!usageRes.ok) {
      return {
        windows: [],
        account: {},
        stale: true,
        error: `usage ${usageRes.status}`,
      }
    }
    const usage = (await usageRes.json()) as Record<string, unknown>
    const profile = profileRes.ok
      ? ((await profileRes.json()) as Record<string, unknown>)
      : {}
    const windows: UsageWindow[] = []
    const five = usage.five_hour as { utilization?: number; resets_at?: string } | undefined
    const seven = usage.seven_day as { utilization?: number; resets_at?: string } | undefined
    if (five) {
      windows.push({
        label: "5h",
        utilization: five.utilization ?? null,
        resets_at: five.resets_at ?? null,
      })
    }
    if (seven) {
      windows.push({
        label: "Week",
        utilization: seven.utilization ?? null,
        resets_at: seven.resets_at ?? null,
      })
    }
    const limits = usage.limits as Array<Record<string, unknown>> | undefined
    if (Array.isArray(limits)) {
      for (const lim of limits) {
        if (lim.kind === "weekly_scoped") {
          const scope = lim.scope as { model?: { display_name?: string } } | undefined
          windows.push({
            label: scope?.model?.display_name || "scoped",
            utilization: (lim.utilization as number) ?? null,
            resets_at: (lim.resets_at as string) ?? null,
          })
        }
      }
    }
    const accountInfo = profile.account as Record<string, unknown> | undefined
    return {
      windows,
      account: {
        email: accountInfo?.email ?? credentialEmail(acc),
        plan_type: (profile.organization as { organization_type?: string } | undefined)
          ?.organization_type,
        rate_limit_tier: (profile.organization as { rate_limit_tier?: string } | undefined)
          ?.rate_limit_tier,
      },
    }
  },
}

function credentialEmail(acc: AcquiredAccount): string | null {
  return acc.credential.email ?? null
}

async function anthropicStreamToOpenAI(res: Response, model: string): Promise<Response> {
  // Defer full conversion: stream OpenAI-shaped chunks via transform when possible.
  // Import stream converter
  const { anthropicSseToOpenAIStream } = await import("../proxy/openai_anthropic")
  if (!res.body) return res
  const out = anthropicSseToOpenAIStream(res.body, model)
  return new Response(out, {
    status: res.status,
    headers: {
      "content-type": "text/event-stream; charset=utf-8",
      "cache-control": "no-cache",
    },
  })
}

