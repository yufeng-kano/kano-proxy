import type { Env } from "../env"
import type { AcquiredAccount } from "../pool/acquire"
import { saveCredential } from "../pool/acquire"
import { mapReasoning } from "../utils/reasoning"
import type { ChatCompletionRequest, ProviderAdapter, UsageWindow } from "./types"
import {
  deleteGrokReasoningReplay,
  grokReasoningReplaySessionKey,
  hashAssistantText,
  readGrokReasoningReplay,
  writeGrokReasoningReplay,
} from "./grok_reasoning_cache"

const XAI_API = "https://api.x.ai/v1"
const CLI_CHAT_PROXY = "https://cli-chat-proxy.grok.com/v1"
const DEFAULT_CLIENT_ID = "b1a00492-073a-47ea-816f-4c329264a828"

/**
 * Official Grok Build CLI client identity — used on the Chat Completions
 * (`api.x.ai`) path. See docs/providers.md.
 */
const GROK_CLI_VERSION = "0.2.117"
const GROK_CLIENT_IDENTIFIER = "grok-shell"
const GROK_USER_AGENT = `${GROK_CLIENT_IDENTIFIER}/${GROK_CLI_VERSION} (linux; x86_64)`

/**
 * CLI chat-proxy identity — used on `/anthropic` → Responses. Matches the
 * working OAuth surface CLIProxyAPI uses (`xai-grok-workspace` + token-auth).
 * Version kept in sync with CPA's `xaiClientVersionValue` (0.2.93).
 */
const GROK_WORKSPACE_VERSION = "0.2.93"
const GROK_WORKSPACE_USER_AGENT = `xai-grok-workspace/${GROK_WORKSPACE_VERSION}`

function grokChatCompletionsHeaders(accessToken: string): Record<string, string> {
  return {
    authorization: `Bearer ${accessToken}`,
    "user-agent": GROK_USER_AGENT,
    "x-grok-client-identifier": GROK_CLIENT_IDENTIFIER,
  }
}

function grokResponsesHeaders(
  accessToken: string,
  affinity?: { convId?: string; sessionId?: string; turnIdx?: string },
): Record<string, string> {
  const headers: Record<string, string> = {
    authorization: `Bearer ${accessToken}`,
    "content-type": "application/json",
    accept: "text/event-stream",
    "user-agent": GROK_WORKSPACE_USER_AGENT,
    "x-grok-client-version": GROK_WORKSPACE_VERSION,
    "X-XAI-Token-Auth": "xai-grok-cli",
  }
  if (affinity?.convId) headers["x-grok-conv-id"] = affinity.convId
  if (affinity?.sessionId) headers["x-grok-session-id"] = affinity.sessionId
  if (affinity?.turnIdx) headers["x-grok-turn-idx"] = affinity.turnIdx
  return headers
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

function affinityFromHeaders(headers: Headers): {
  convId?: string
  sessionId?: string
  turnIdx?: string
} {
  return {
    convId: headers.get("x-grok-conv-id") ?? undefined,
    sessionId: headers.get("x-grok-session-id") ?? undefined,
    turnIdx: headers.get("x-grok-turn-idx") ?? undefined,
  }
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
          ...grokChatCompletionsHeaders(acc.credential.access_token),
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

  /**
   * OpenAI surface: Chat Completions on api.x.ai (unchanged wire format).
   * Anthropic surface uses `messages()` → Responses instead.
   */
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
    // Chat Completions still needs this flag for plaintext reasoning_content
    // when the egress gate allows it (docs/api.md).
    body.include_reasoning = true

    const headers: Record<string, string> = {
      ...grokChatCompletionsHeaders(acc.credential.access_token),
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

  /**
   * Anthropic surface: Messages ↔ cli-chat-proxy Responses with encrypted
   * reasoning. Route sets x-kano-api-key-id for replay-cache isolation.
   */
  async messages(env, account, body, headers, extras) {
    const acc = await refreshGrok(env, account)
    const {
      anthropicToGrokResponses,
      collectGrokResponsesSseToAnthropic,
      grokResponsesSseToAnthropicStream,
      lastAssistantTextFromAnthropicMessages,
      InvalidGrokReasoningEffortError,
    } = await import("../proxy/grok_anthropic")

    const reqBody = body as Record<string, unknown>
    const affinity = affinityFromHeaders(headers)
    const apiKeyId = headers.get("x-kano-api-key-id")?.trim() || ""
    const sessionKey = grokReasoningReplaySessionKey(affinity)
    const rawModel = String(reqBody.model ?? "")
    // Route already rewrote model to bare upstream id for native adapters;
    // keep a display id if the client sent provider/model.
    const upstreamModel = rawModel.includes("/")
      ? rawModel.slice(rawModel.indexOf("/") + 1)
      : rawModel
    const displayModel = headers.get("x-kano-raw-model")?.trim() || `grok/${upstreamModel}`
    const waitUntil = extras?.waitUntil

    let replayEncrypted: string | null = null
    if (apiKeyId && sessionKey) {
      const thinking = reqBody.thinking as { type?: string } | undefined
      const disabled =
        thinking &&
        typeof thinking === "object" &&
        String(thinking.type ?? "").toLowerCase() === "disabled"
      // Empty assistant text (tool-only turns) shares one hash — refuse replay
      // match rather than risk injecting another turn's ciphertext.
      const lastText = lastAssistantTextFromAnthropicMessages(reqBody)
      if (!disabled && lastText) {
        const cached = await readGrokReasoningReplay(
          env,
          apiKeyId,
          upstreamModel,
          sessionKey,
        )
        if (cached) {
          const lastHash = await hashAssistantText(lastText)
          if (lastHash === cached.assistant_text_hash) {
            replayEncrypted = cached.encrypted_content
          }
        }
      }
    }

    let converted
    try {
      converted = anthropicToGrokResponses(reqBody, {
        upstreamModel,
        replayEncryptedContent: replayEncrypted,
      })
    } catch (e) {
      if (e instanceof InvalidGrokReasoningEffortError) {
        return Response.json(
          {
            type: "error",
            error: {
              type: "invalid_request_error",
              message: "invalid reasoning_effort",
            },
          },
          { status: 400 },
        )
      }
      throw e
    }

    const stream = !!reqBody.stream
    const res = await fetch(`${CLI_CHAT_PROXY}/responses`, {
      method: "POST",
      headers: grokResponsesHeaders(acc.credential.access_token, affinity),
      body: JSON.stringify(converted.body),
    })

    if (!res.ok) {
      const t = await res.text()
      return new Response(t, {
        status: res.status,
        headers: {
          "content-type": res.headers.get("content-type") || "application/json",
        },
      })
    }
    if (!res.body) return res

    const schedule = (p: Promise<unknown>) => {
      if (waitUntil) waitUntil(p)
      else void p
    }
    const onTurnOutcome = (outcome: {
      kind: "replayable" | "clear"
      encrypted_content?: string
      assistant_text?: string
    }) => {
      if (!apiKeyId || !sessionKey) return
      if (outcome.kind === "clear") {
        schedule(deleteGrokReasoningReplay(env, apiKeyId, upstreamModel, sessionKey))
        return
      }
      // Skip persisting tool-only (empty text) turns — match would be ambiguous.
      if (!outcome.encrypted_content || !outcome.assistant_text) return
      schedule(
        (async () => {
          const assistant_text_hash = await hashAssistantText(outcome.assistant_text!)
          await writeGrokReasoningReplay(env, apiKeyId, upstreamModel, sessionKey, {
            encrypted_content: outcome.encrypted_content!,
            assistant_text_hash,
          })
        })(),
      )
    }

    if (stream) {
      const anthropicStream = grokResponsesSseToAnthropicStream(res.body, displayModel, {
        thinkingMode: converted.thinkingMode,
        onTurnOutcome,
      })
      return new Response(anthropicStream, {
        status: 200,
        headers: {
          "content-type": "text/event-stream; charset=utf-8",
          "cache-control": "no-cache",
        },
      })
    }

    const msg = await collectGrokResponsesSseToAnthropic(res.body, displayModel, {
      thinkingMode: converted.thinkingMode,
      onTurnOutcome,
    })
    if ("error" in msg) {
      return Response.json(
        { type: "error", error: msg.error },
        { status: 502 },
      )
    }
    return Response.json(msg)
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
