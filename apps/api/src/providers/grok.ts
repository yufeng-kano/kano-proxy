import type { Env } from "../env"
import type { AcquiredAccount } from "../pool/acquire"
import { saveCredential } from "../pool/acquire"
import { mapReasoning } from "../utils/reasoning"
import type { ChatCompletionRequest, ProviderAdapter, UsageWindow } from "./types"

const XAI_API = "https://api.x.ai/v1"
const DEFAULT_CLIENT_ID = "b1a00492-073a-47ea-816f-4c329264a828"

/**
 * Official Grok Build CLI client identity.
 *
 * The subscription OAuth surface is the CLI's, so we present as the CLI —
 * same reason the codex adapter sends `originator: codex_cli_rs`. xAI buckets
 * traffic server-side by these values (see xai-org/grok-build
 * `xai-grok-pager/src/client_identity.rs`), and providers on this kind of
 * surface have gated on client shape before (chatgpt.com's bot wall on
 * /codex/usage). Shape and version verified against
 * `xai-grok-sampler/src/client.rs` + `xai-grok-version` 2026-08-01.
 *
 * This does NOT change which billing pool usage lands in — nothing in the
 * source ties these headers to metering. The prompt-cache win comes from
 * `x-grok-conv-id` below.
 */
const GROK_CLI_VERSION = "0.2.117"
const GROK_CLIENT_IDENTIFIER = "grok-shell"
const GROK_USER_AGENT = `${GROK_CLIENT_IDENTIFIER}/${GROK_CLI_VERSION} (linux; x86_64)`

function grokClientHeaders(accessToken: string): Record<string, string> {
  return {
    authorization: `Bearer ${accessToken}`,
    "user-agent": GROK_USER_AGENT,
    "x-grok-client-identifier": GROK_CLIENT_IDENTIFIER,
  }
}

function clientId(env: Env): string {
  return env.GROK_OAUTH_CLIENT_ID || DEFAULT_CLIENT_ID
}

async function refreshGrok(env: Env, account: AcquiredAccount): Promise<AcquiredAccount> {
  const { credential } = account
  if (!credential.refresh_token || !credential.token_endpoint) return account
  const exp = credential.expires_at ? Date.parse(credential.expires_at) : 0
  if (exp && exp - 3_600_000 > Date.now()) return account

  const res = await fetch(credential.token_endpoint, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
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

export const grokAdapter: ProviderAdapter = {
  id: "grok",

  async refreshIfNeeded(env, account) {
    return refreshGrok(env, account)
  },

  async listModels(env, account) {
    const acc = await refreshGrok(env, account)
    try {
      const res = await fetch(`${XAI_API}/models`, {
        headers: {
          ...grokClientHeaders(acc.credential.access_token),
          accept: "application/json",
        },
      })
      if (!res.ok) {
        return { models: [], error: `models ${res.status}` }
      }
      const json = (await res.json()) as {
        data?: Array<{ id?: string; name?: string }>
      }
      const models = (json.data ?? [])
        .filter((m) => typeof m.id === "string" && m.id)
        .map((m) => ({
          id: m.id as string,
          display_name: (typeof m.name === "string" && m.name) || null,
        }))
      return { models, error: null }
    } catch (e) {
      return {
        models: [],
        error: e instanceof Error ? e.message : "models fetch failed",
      }
    }
  },

  async chatCompletions(env, account, req) {
    const acc = await refreshGrok(env, account)
    const mapped = mapReasoning("grok", req.reasoning_effort)

    const body: Record<string, unknown> = {
      model: req.upstreamModel,
      messages: req.messages,
      stream: !!req.stream,
    }
    if (req.max_tokens != null) body.max_tokens = req.max_tokens
    if (req.tools) body.tools = req.tools
    if (req.tool_choice) body.tool_choice = req.tool_choice
    if (req.response_format) body.response_format = req.response_format
    if (req.stop?.length) body.stop = req.stop
    if (mapped.reasoning_effort) body.reasoning_effort = mapped.reasoning_effort
    // Ask for the final usage chunk: without it every converted Anthropic
    // response reports input_tokens 0 and clients cannot track context.
    if (req.stream) body.stream_options = { include_usage: true }
    // Pin the surface default explicitly: Anthropic and OpenAI both document
    // `1`, xAI's own default is unspecified (docs/providers.md "Sampling").
    body.temperature = req.temperature ?? 1
    if (req.top_p != null) body.top_p = req.top_p
    // Reasoning is hidden upstream unless asked for — without this, xAI
    // returns only usage.completion_tokens_details.reasoning_tokens, no
    // text (verified 2026-08-02). See docs/api.md "Grok reasoning".
    body.include_reasoning = true

    const headers: Record<string, string> = {
      ...grokClientHeaders(acc.credential.access_token),
      "content-type": "application/json",
      // Per-request id, like the CLI's x_grok_req_id.
      "x-grok-req-id": crypto.randomUUID(),
      "x-grok-model-override": req.upstreamModel,
    }
    // Sticky routing for prompt cache — only when the client supplied an id.
    const aff = req.affinity
    if (aff?.convId) headers["x-grok-conv-id"] = aff.convId
    if (aff?.sessionId) headers["x-grok-session-id"] = aff.sessionId
    if (aff?.turnIdx) headers["x-grok-turn-idx"] = aff.turnIdx

    const res = await fetch(`${XAI_API}/chat/completions`, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
    })
    return res
  },

  async fetchUsage(env, account) {
    const acc = await refreshGrok(env, account)
    // Unofficial SuperGrok billing surface
    try {
      const userRes = await fetch("https://cli-chat-proxy.grok.com/v1/user", {
        headers: {
          authorization: `Bearer ${acc.credential.access_token}`,
          accept: "application/json",
        },
      })
      let userId: string | undefined
      if (userRes.ok) {
        const u = (await userRes.json()) as { userId?: string; id?: string }
        userId = u.userId ?? u.id
      }
      const headers: Record<string, string> = {
        authorization: `Bearer ${acc.credential.access_token}`,
        accept: "application/json",
      }
      if (userId) headers["x-userid"] = userId
      const billRes = await fetch(
        "https://cli-chat-proxy.grok.com/v1/billing?format=credits",
        { headers },
      )
      if (!billRes.ok) {
        return {
          windows: [],
          account: { email: acc.credential.email ?? null },
          stale: true,
          error: `billing ${billRes.status}`,
        }
      }
      const bill = (await billRes.json()) as {
        config?: {
          creditUsagePercent?: number
          currentPeriod?: { end?: string; type?: string }
        }
        subscriptionTier?: string
        subscriptionTiers?: string
      }
      const pct = bill.config?.creditUsagePercent
      const end = bill.config?.currentPeriod?.end ?? null
      const windows: UsageWindow[] = [
        {
          label: "Week",
          utilization: pct ?? null,
          resets_at: end,
        },
      ]
      return {
        windows,
        account: {
          email: acc.credential.email ?? null,
          plan_type: bill.subscriptionTier ?? bill.subscriptionTiers ?? null,
        },
      }
    } catch (e) {
      return {
        windows: [],
        account: {},
        stale: true,
        error: e instanceof Error ? e.message : "billing failed",
      }
    }
  },
}
