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

export function betaHeaders(extra?: string | null): string {
  const base = [
    "oauth-2025-04-20",
    "claude-code-20250219",
    "interleaved-thinking-2025-05-14",
    "fine-grained-tool-streaming-2025-05-14",
  ]
  if (extra) {
    for (const p of extra.split(",")) {
      const t = p.trim()
      if (t && !base.includes(t)) base.push(t)
    }
  }
  return base.join(",")
}

/**
 * anthropic-beta header for the native /anthropic passthrough. A client body
 * carrying `output_config` needs the effort beta upstream, or Anthropic
 * rejects/ignores `output_config` — add it automatically, deduped the same
 * way client-supplied extras are (so a client that already sent it is not
 * doubled).
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
    if (mapped.error) {
      return Response.json(
        { error: { message: mapped.error, code: "invalid_reasoning" } },
        { status: 400 },
      )
    }
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
    })
    // OpenAI→Claude: do NOT add cache_control
    const withSystem = prependRequiredSystem(anthropicBody)
    const res = await fetch(`${ANTHROPIC_API}/v1/messages`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${acc.credential.access_token}`,
        "content-type": "application/json",
        "anthropic-version": "2023-06-01",
        "anthropic-beta": betaHeaders(EFFORT_BETA),
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

