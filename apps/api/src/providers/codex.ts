import type { Env } from "../env"
import type { AcquiredAccount } from "../pool/acquire"
import { mapReasoning } from "../utils/reasoning"
import { refreshOAuthCredential } from "./refresh"
import type { CodexReasoningReplayItem } from "./codex_reasoning_cache"
import type { ChatCompletionRequest, ProviderAdapter, UsageWindow } from "./types"

const TOKEN_URL = "https://auth.openai.com/oauth/token"
const DEFAULT_CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann"

export const CODEX_USER_AGENT =
  "codex-tui/0.146.0 (Mac OS 26.5.0; arm64) iTerm.app/3.6.10 (codex-tui; 0.146.0)"
export const CODEX_ORIGINATOR = "codex-tui"

function clientId(env: Env): string {
  return env.CODEX_OAUTH_CLIENT_ID || DEFAULT_CLIENT_ID
}

async function refreshCodex(env: Env, account: AcquiredAccount): Promise<AcquiredAccount> {
  return refreshOAuthCredential(
    env,
    account,
    (credential) => {
      if (!credential.refresh_token) return false
      const exp = credential.expires_at ? Date.parse(credential.expires_at) : 0
      return !exp || exp - 60_000 <= Date.now()
    },
    async (credential) => {
      const res = await fetch(TOKEN_URL, {
        method: "POST",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({
          grant_type: "refresh_token",
          refresh_token: credential.refresh_token!,
          client_id: credential.client_id || clientId(env),
        }),
      })
      if (!res.ok) return null
      const json = (await res.json()) as { access_token: string; refresh_token?: string; expires_in?: number }
      return {
        ...credential,
        access_token: json.access_token,
        refresh_token: json.refresh_token ?? credential.refresh_token,
        expires_at: json.expires_in
          ? new Date(Date.now() + json.expires_in * 1000).toISOString()
          : credential.expires_at,
      }
    },
  )
}

function accountIdFromJwt(access: string): string | null {
  try {
    const payload = access.split(".")[1]
    if (!payload) return null
    const json = JSON.parse(atob(payload.replace(/-/g, "+").replace(/_/g, "/")))
    const auth = json["https://api.openai.com/auth"] as { chatgpt_account_id?: string } | undefined
    return auth?.chatgpt_account_id ?? credentialAccount(json)
  } catch {
    return null
  }
}

function credentialAccount(_json: unknown): string | null {
  return null
}

const SESSION_UUID_SUFFIX = /_session_([0-9a-fA-F-]{36})$/

/**
 * Candidate `session_id` header value from the request's prompt_cache_key
 * (client-sent on /openai/v1, metadata.user_id-derived on /anthropic).
 * A key ending in `_session_<uuid>` contributes that bare UUID, matching
 * what the Codex CLI itself puts in this header; any other non-empty key is
 * used as-is. The header only needs to be stable per conversation. The
 * caller still fits the winning value — see the `session_id` header below.
 */
export function codexSessionId(promptCacheKey?: string): string | null {
  const key = promptCacheKey?.trim()
  if (!key) return null
  const uuid = SESSION_UUID_SUFFIX.exec(key)?.[1]
  return uuid ?? key
}

/** OpenAI's documented `prompt_cache_key` ceiling; over it is a hard 400. */
const CODEX_PROMPT_CACHE_KEY_MAX = 64

/**
 * Fit a cache/session identifier into the Responses `prompt_cache_key`
 * 64-char limit. Applies to BOTH the body field and the `session_id` header:
 * the backend validates the header under the `prompt_cache_key` name, so an
 * over-long header 400s the turn (`Invalid 'prompt_cache_key': string too
 * long`) even when the body field is already fitted. Must be deterministic —
 * a conversation only gets upstream cache hits if its value is stable.
 * Long keys are hashed rather than truncated: client ids share long fixed
 * prefixes, so a prefix cut would collapse every session of one account onto
 * a single cache shard.
 */
export async function fitCodexPromptCacheKey(key: string): Promise<string> {
  const uuid = SESSION_UUID_SUFFIX.exec(key)?.[1]
  if (uuid) return uuid
  if (key.length <= CODEX_PROMPT_CACHE_KEY_MAX) return key
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(key))
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("")
}

/**
 * The `session_id` header value actually sent upstream: client affinity ids
 * first, then the prompt_cache_key-derived candidate, then a per-request
 * random UUID — each fitted to the 64-char ceiling above.
 *
 * Every candidate is fitted, not just the derived one: affinity ids are
 * opaque client input and are just as capable of exceeding the limit.
 * Fitting is a no-op for anything already ≤ 64 chars, so short ids (the
 * Codex CLI's bare UUID included) still go out verbatim.
 */
export async function codexSessionIdHeader(req: {
  affinity?: { sessionId?: string; convId?: string }
  prompt_cache_key?: string
}): Promise<string> {
  const candidate =
    req.affinity?.sessionId || req.affinity?.convId || codexSessionId(req.prompt_cache_key)
  if (!candidate) return crypto.randomUUID()
  return fitCodexPromptCacheKey(candidate)
}

function windowLabel(seconds: number | undefined): string {
  if (!seconds) return "window"
  if (seconds === 604800) return "Week"
  if (seconds % 3600 === 0 && seconds < 86400) return `${seconds / 3600}h`
  if (seconds % 86400 === 0) return `${seconds / 86400}d`
  return `${seconds}s`
}

export const codexAdapter: ProviderAdapter = {
  id: "codex",

  async refreshIfNeeded(env, account) {
    return refreshCodex(env, account)
  },

  async listModels(env, account) {
    const acc = await refreshCodex(env, account)
    const chatgptAccountId =
      acc.credential.account_id || accountIdFromJwt(acc.credential.access_token) || ""
    const { fetchCodexModels } = await import("./codex_models")
    const { codexUpstream } = await import("./codex_relay")
    return fetchCodexModels(acc.credential.access_token, chatgptAccountId, codexUpstream(env))
  },

  async chatCompletions(env, account, req, extras) {
    const acc = await refreshCodex(env, account)
    const mapped = mapReasoning("codex", req.reasoning_effort)
    const chatgptAccountId =
      acc.credential.account_id || accountIdFromJwt(acc.credential.access_token) || ""

    if (req.responsesBody) {
      return codexNativeResponses(env, acc, chatgptAccountId, req, extras, mapped.reasoning)
    }

    const {
      codexReasoningReplaySessionKey,
      readCodexReasoningReplay,
      writeCodexReasoningReplay,
      deleteCodexReasoningReplay,
      hashAssistantText,
    } = await import("./codex_reasoning_cache")
    const apiKeyId = extras?.apiKeyId ?? ""
    const sessionKey = codexReasoningReplaySessionKey(req.affinity, req.prompt_cache_key)
    const replayScoped = !!apiKeyId && !!sessionKey

    const body = await buildCodexRequestBody(req, mapped.reasoning)

    // Re-inject the previous turn's reasoning items. `store: false` means the
    // upstream keeps nothing, and neither wire format the client speaks can
    // carry an opaque Responses reasoning item — so without this the model
    // re-reasons from scratch every tool round. Only replay when the previous
    // assistant text still matches what produced them; a mismatch means the
    // client edited history and the items no longer belong to this turn.
    if (replayScoped) {
      const cached = await readCodexReasoningReplay(env, apiKeyId, req.upstreamModel, sessionKey!)
      if (cached) {
        const priorText = lastAssistantText(req.messages)
        if (priorText && (await hashAssistantText(priorText)) === cached.assistant_text_hash) {
          body.input = mergeCodexReplayItems(body.input, cached.items)
        }
      }
    }
    const headers: Record<string, string> = {
      authorization: `Bearer ${acc.credential.access_token}`,
      "user-agent": CODEX_USER_AGENT,
      originator: CODEX_ORIGINATOR,
      connection: "Keep-Alive",
      session_id: await codexSessionIdHeader(req),
      "content-type": "application/json",
      accept: "text/event-stream",
    }
    if (chatgptAccountId) headers["chatgpt-account-id"] = chatgptAccountId

    const { codexUpstream, relayFetch } = await import("./codex_relay")
    const upstream = codexUpstream(env)
    const res = await relayFetch(upstream, `${upstream.base}/codex/responses`, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
      signal: extras?.signal,
    })

    if (!res.ok) {
      const t = await res.text()
      return new Response(t, {
        status: res.status,
        headers: { "content-type": res.headers.get("content-type") || "application/json" },
      })
    }

    // Persist this turn's reasoning for the next one. A completed turn with
    // nothing replayable clears the entry rather than leaving a stale one that
    // would be re-injected against a conversation that has moved on.
    const schedule = (p: Promise<unknown>) => {
      if (extras?.waitUntil) extras.waitUntil(p)
      else void p
    }
    const onReplayItems = replayScoped
      ? (items: CodexReasoningReplayItem[], assistantText: string) => {
          if (items.length === 0) {
            schedule(deleteCodexReasoningReplay(env, apiKeyId, req.upstreamModel, sessionKey!))
            return
          }
          schedule(
            hashAssistantText(assistantText).then((assistant_text_hash) =>
              writeCodexReasoningReplay(env, apiKeyId, req.upstreamModel, sessionKey!, {
                items,
                assistant_text_hash,
              }),
            ),
          )
        }
      : undefined

    if (req.stream) {
      if (!res.body) return res
      const { codexSseToOpenAIStream } = await import("../proxy/codex_openai")
      return new Response(codexSseToOpenAIStream(res.body, req.upstreamModel, { onReplayItems }), {
        status: 200,
        headers: {
          "content-type": "text/event-stream; charset=utf-8",
          "cache-control": "no-cache",
        },
      })
    }

    // Non-stream: consume SSE and build one completion
    if (!res.body) return res
    const { collectCodexSse } = await import("../proxy/codex_openai")
    const openai = await collectCodexSse(res.body, req.upstreamModel, { onReplayItems })
    if ("error" in openai) {
      // response.failed / error mid-turn: never fabricate a 200 completion.
      return Response.json(openai, { status: 502 })
    }
    return Response.json(openai)
  },

  async fetchUsage(env, account) {
    const acc = await refreshCodex(env, account)
    const chatgptAccountId =
      acc.credential.account_id || accountIdFromJwt(acc.credential.access_token) || ""
    const { fetchCodexUsageJson, windowsFromCodexPayload } = await import("./codex_usage")
    const { codexUpstream } = await import("./codex_relay")
    const result = await fetchCodexUsageJson(
      acc.credential.access_token,
      chatgptAccountId,
      codexUpstream(env),
    )

    if (result.ok && result.payload) {
      const windows: UsageWindow[] = windowsFromCodexPayload(result.payload)
      return {
        windows,
        account: {
          email: result.payload.email ?? acc.credential.email ?? null,
          plan_type: result.payload.plan_type ?? null,
          account_id: chatgptAccountId || null,
        },
        stale: false,
        error: null,
      }
    }

    // Usage endpoint often 403s from CF Workers (edge bot wall). Account is still usable.
    return {
      windows: [],
      account: {
        email: acc.credential.email ?? null,
        account_id: chatgptAccountId || null,
      },
      stale: true,
      error: result.error,
      edgeBlocked: result.edgeBlocked,
    }
  },
}

/**
 * Pure builder for the `/codex/responses` request body — no network, so it is
 * unit-testable directly. `reasoning` is passed in already mapped (the
 * adapter resolves/validates `reasoning_effort` before calling this).
 */
type CodexRequestBodyInput = Pick<
  ChatCompletionRequest,
  "upstreamModel" | "messages" | "tools" | "tool_choice" | "response_format" | "prompt_cache_key"
> & {
  /** Kept only when it is the one service tier the Responses backend accepts. */
  service_tier?: unknown
  /** The route retains the raw client body; read only service_tier from it. */
  rawBody?: Record<string, unknown>
}

function codexServiceTier(req: CodexRequestBodyInput): unknown {
  if (req.service_tier !== undefined) return req.service_tier
  return req.rawBody?.service_tier
}

export async function buildCodexRequestBody(
  req: CodexRequestBodyInput,
  reasoning?: { effort: string; summary: "auto" },
): Promise<Record<string, unknown>> {
  const instructions = extractSystemInstructions(req.messages)
  // System messages are hoisted into `instructions`; filter them before the
  // input mapper so its defensive system→developer branch cannot duplicate
  // the top-level instructions item.
  const inputMessages = req.messages.filter((message) => {
    if (!message || typeof message !== "object") return true
    return String((message as { role?: unknown }).role ?? "") !== "system"
  })
  // `tools: []` counts as no tools — upstream may reject tool_choice without tools.
  const hasTools = Array.isArray(req.tools) && req.tools.length > 0
  const body: Record<string, unknown> = {
    model: req.upstreamModel,
    input: await openaiMessagesToCodexInput(inputMessages),
    stream: true, // codex backend is SSE-oriented
    store: false,
    include: ["reasoning.encrypted_content"],
  }
  if (instructions) body.instructions = instructions
  if (reasoning) body.reasoning = reasoning
  if (hasTools) {
    body.tools = mapTools(req.tools)
    body.parallel_tool_calls = true
  }
  const toolChoice = mapCodexToolChoice(req.tool_choice, hasTools)
  if (toolChoice !== undefined) body.tool_choice = toolChoice
  if (req.response_format) {
    const rf = req.response_format as {
      type?: string
      json_schema?: { name?: string; schema?: unknown; strict?: boolean }
    }
    if (rf.type === "json_schema" && rf.json_schema?.schema) {
      body.text = {
        format: {
          type: "json_schema",
          name: rf.json_schema.name || "response",
          schema: rf.json_schema.schema,
          strict: rf.json_schema.strict ?? false,
        },
      }
    }
  }
  // Both wire-bound values are fitted (here and in codexSessionIdHeader);
  // only the KV replay session key still uses the full client value, since
  // it never leaves this Worker.
  if (req.prompt_cache_key) {
    body.prompt_cache_key = await fitCodexPromptCacheKey(req.prompt_cache_key)
  }
  const serviceTier = codexServiceTier(req)
  if (serviceTier !== undefined) body.service_tier = serviceTier
  return stripRejectedCodexFields(body)
}

const CODEX_REJECTED_BODY_FIELDS = [
  "max_output_tokens",
  "max_completion_tokens",
  "temperature",
  "top_p",
  "truncation",
  "user",
  "previous_response_id",
  "generate",
  "prompt_cache_retention",
  "safety_identifier",
  "stream_options",
] as const

/** Remove fields rejected by `/codex/responses`, without touching valid fields. */
function stripRejectedCodexFields(body: Record<string, unknown>): Record<string, unknown> {
  const cleaned = { ...body }
  for (const field of CODEX_REJECTED_BODY_FIELDS) delete cleaned[field]
  if (cleaned.service_tier !== "priority") delete cleaned.service_tier
  return cleaned
}

/**
 * Responses rejects a `call_id` over 64 chars, and Claude Code emits tool ids
 * long enough to hit that. Shorten to a 64-char prefix plus a hash suffix,
 * matching the reference proxy so a `function_call` and its
 * `function_call_output` still resolve to the same id.
 */
async function shortenCodexCallId(id: unknown): Promise<unknown> {
  if (typeof id !== "string" || id.length <= 64) return id
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(id))
  const hex = Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("")
  const suffix = `_${hex.slice(0, 16)}`
  return id.slice(0, 64 - suffix.length) + suffix
}

/**
 * OpenAI Chat Completions `tool_choice` → Responses flattened shape.
 * Upstream may reject `tool_choice` sent without `tools`, so callers must
 * gate `hasTools` themselves and drop an `undefined` result.
 */
function mapCodexToolChoice(toolChoice: unknown, hasTools: boolean): unknown {
  if (!hasTools) return undefined
  if (toolChoice == null) return "auto"
  if (toolChoice === "auto" || toolChoice === "none" || toolChoice === "required") {
    return toolChoice
  }
  if (typeof toolChoice === "object") {
    const tc = toolChoice as { type?: string; function?: { name?: string } }
    if (tc.type === "function" && tc.function?.name) {
      return { type: "function", name: tc.function.name }
    }
  }
  return toolChoice
}

/**
 * `role: "system"` messages are pulled out of the message list entirely —
 * they become the top-level Responses `instructions` field (see
 * `extractSystemInstructions`), not fake `role: "user"` input items.
 */
async function openaiMessagesToCodexInput(messages: unknown[]): Promise<unknown[]> {
  const input: unknown[] = []
  for (const m of messages as Array<Record<string, unknown>>) {
    const role = String(m.role ?? "")
    if (role === "system") {
      // The normal builder hoists system messages into top-level instructions
      // before calling this mapper. Keep this defensive path Responses-valid if
      // a system item reaches the input mapper from another caller.
      input.push({
        role: "developer",
        content: contentToCodex(m.content),
      })
      continue
    }
    if (role === "user") {
      input.push({
        role: "user",
        content: contentToCodex(m.content),
      })
      continue
    }
    if (role === "assistant") {
      // Text precedes the tool calls it led up to — matches the order the
      // original message carried them in.
      const text = contentText(m.content)
      if (text) {
        input.push({
          role: "assistant",
          content: [{ type: "output_text", text }],
        })
      }
      const toolCalls = m.tool_calls as Array<Record<string, unknown>> | undefined
      if (toolCalls?.length) {
        for (const tc of toolCalls) {
          const fn = tc.function as { name?: string; arguments?: string }
          input.push({
            type: "function_call",
            call_id: await shortenCodexCallId(tc.id),
            name: fn?.name,
            arguments: fn?.arguments ?? "{}",
          })
        }
      }
      continue
    }
    if (role === "tool") {
      input.push({
        type: "function_call_output",
        call_id: await shortenCodexCallId(m.tool_call_id),
        output: contentText(m.content),
      })
    }
  }
  return input
}

/**
 * Text of the most recent assistant message, used to confirm cached reasoning
 * items still belong to this conversation before replaying them. Scans past
 * whatever trails it (the new user turn, tool results), matching the grok
 * cache's `lastAssistantTextFromAnthropicMessages`. An empty result — a
 * tool-only turn — refuses the match rather than sharing one hash across
 * every such turn.
 */
export function lastAssistantText(messages: unknown[]): string {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i] as Record<string, unknown> | null
    if (!m || typeof m !== "object") continue
    if (String(m.role ?? "") !== "assistant") continue
    return contentText(m.content)
  }
  return ""
}

/**
 * Splice cached reasoning items into the Responses `input` ahead of the turn
 * they belong to, skipping anything the client already replayed itself.
 * Blind prepending would double up `function_call` items whenever the client
 * echoes its own tool history (Claude Code does), which upstream rejects.
 */
export function mergeCodexReplayItems(input: unknown, cachedItems: unknown[]): unknown[] {
  const items = Array.isArray(input) ? (input as Array<Record<string, unknown>>) : []
  if (!Array.isArray(cachedItems) || cachedItems.length === 0) return items

  const existingCallIds = new Set<string>()
  let hasReasoning = false
  for (const item of items) {
    if (!item || typeof item !== "object") continue
    const type = String(item.type ?? "")
    if (type === "reasoning") hasReasoning = true
    if (type === "function_call" || type === "custom_tool_call") {
      const id = typeof item.call_id === "string" ? item.call_id : ""
      if (id) existingCallIds.add(id)
    }
  }

  const replay: unknown[] = []
  for (const raw of cachedItems) {
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) continue
    const item = raw as Record<string, unknown>
    const type = String(item.type ?? "")
    // The client already carries its own reasoning — ours would conflict.
    if (type === "reasoning" && hasReasoning) continue
    if (type === "function_call" || type === "custom_tool_call") {
      const id = typeof item.call_id === "string" ? item.call_id : ""
      if (id && existingCallIds.has(id)) continue
    }
    replay.push(item)
  }
  if (replay.length === 0) return items

  // Anchor before the first tool result: that is where the prior assistant
  // turn ended, so its reasoning must sit ahead of the results it produced.
  const anchor = items.findIndex(
    (i) =>
      i &&
      typeof i === "object" &&
      (String(i.type ?? "") === "function_call_output" ||
        String(i.type ?? "") === "custom_tool_call_output"),
  )
  if (anchor < 0) return [...items, ...replay]
  return [...items.slice(0, anchor), ...replay, ...items.slice(anchor)]
}

/** Join every `role: "system"` message's text, in order, with a blank line. */
function extractSystemInstructions(messages: unknown[]): string {
  const chunks: string[] = []
  for (const m of messages as Array<Record<string, unknown>>) {
    if (String(m.role ?? "") !== "system") continue
    const text = contentText(m.content)
    if (text) chunks.push(text)
  }
  return chunks.join("\n\n")
}

function contentText(content: unknown): string {
  if (typeof content === "string") return content
  if (Array.isArray(content)) {
    return content
      .map((p) =>
        typeof p === "object" && p && (p as { type?: string }).type === "text"
          ? String((p as { text?: string }).text ?? "")
          : typeof p === "string"
            ? p
            : "",
      )
      .join("")
  }
  return ""
}

function contentToCodex(content: unknown): unknown[] {
  if (typeof content === "string") return [{ type: "input_text", text: content }]
  if (!Array.isArray(content)) return [{ type: "input_text", text: String(content ?? "") }]
  const out: unknown[] = []
  for (const p of content as Array<Record<string, unknown>>) {
    if (p.type === "text") out.push({ type: "input_text", text: p.text })
    else if (p.type === "image_url") {
      const url = (p.image_url as { url?: string })?.url
      if (url) out.push({ type: "input_image", image_url: url })
    }
  }
  return out.length ? out : [{ type: "input_text", text: "" }]
}

function mapTools(tools: unknown): unknown[] {
  if (!Array.isArray(tools)) return []
  return tools.map((t) => {
    const tr = t as { function?: { name?: string; description?: string; parameters?: unknown } }
    if (tr.function) {
      return {
        type: "function",
        name: tr.function.name,
        description: tr.function.description,
        parameters: tr.function.parameters,
      }
    }
    return t
  })
}

// ---------------------------------------------------------------------------
// Native Responses ingress (`POST /openai/v1/responses`, docs/api.md)
// ---------------------------------------------------------------------------

function isRecord(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && !Array.isArray(v)
}

function responsesItemText(content: unknown): string {
  if (typeof content === "string") return content
  if (!Array.isArray(content)) return ""
  const out: string[] = []
  for (const part of content) {
    if (typeof part === "string") out.push(part)
    else if (isRecord(part) && typeof part.text === "string") out.push(part.text)
  }
  return out.join("\n\n")
}

const CALL_ID_ITEM_TYPES = new Set([
  "function_call",
  "function_call_output",
  "custom_tool_call",
  "custom_tool_call_output",
])

/**
 * The client's own Responses body, fitted for `/codex/responses` with the
 * same fix-ups the Chat path applies (docs/api.md "Native path"): system
 * items hoisted into `instructions`, `store: false` / `stream: true`
 * forced, `include` guaranteed, effort clamped, call_ids shortened,
 * `prompt_cache_key` fitted, tool_choice dropped without tools, rejected
 * fields stripped. Everything else — client-echoed reasoning items, hosted
 * tools, namespace groups, `text`, `client_metadata` — rides through
 * untouched. Pure: no network, unit-testable directly.
 */
export async function buildCodexNativeRequestBody(
  client: Record<string, unknown>,
  opts: {
    upstreamModel: string
    /** Already mapped/clamped by the adapter; undefined when the client sent no effort, or `none`. */
    reasoning?: { effort: string; summary: "auto" }
  },
): Promise<Record<string, unknown>> {
  const body: Record<string, unknown> = { ...client, model: opts.upstreamModel, stream: true, store: false }

  const rawInput = body.input
  const items: unknown[] =
    typeof rawInput === "string"
      ? [{ type: "message", role: "user", content: rawInput }]
      : Array.isArray(rawInput)
        ? rawInput
        : []
  const system: string[] = []
  const input: unknown[] = []
  for (const item of items) {
    if (!isRecord(item)) continue
    if (item.role === "system" && (item.type === "message" || item.type === undefined)) {
      const text = responsesItemText(item.content)
      if (text) system.push(text)
      continue
    }
    if (typeof item.type === "string" && CALL_ID_ITEM_TYPES.has(item.type) && typeof item.call_id === "string") {
      input.push({ ...item, call_id: await shortenCodexCallId(item.call_id) })
      continue
    }
    input.push(item)
  }
  const instructions = [typeof body.instructions === "string" ? body.instructions : "", ...system]
    .filter(Boolean)
    .join("\n\n")
  if (instructions) body.instructions = instructions
  else delete body.instructions
  body.input = input

  const include = Array.isArray(body.include)
    ? body.include.filter((s): s is string => typeof s === "string")
    : []
  if (!include.includes("reasoning.encrypted_content")) include.push("reasoning.encrypted_content")
  body.include = include

  const clientReasoning = isRecord(body.reasoning) ? { ...body.reasoning } : undefined
  if (opts.reasoning) {
    body.reasoning = {
      ...(clientReasoning ?? {}),
      effort: opts.reasoning.effort,
      summary: clientReasoning?.summary ?? opts.reasoning.summary,
    }
  } else if (clientReasoning && clientReasoning.effort !== undefined) {
    // Effort `none` (or unmapped): drop the effort but keep the client's
    // other reasoning settings, e.g. the summary mode the CLI asked for.
    delete clientReasoning.effort
    if (Object.keys(clientReasoning).length) body.reasoning = clientReasoning
    else delete body.reasoning
  }

  const hasTools = Array.isArray(body.tools) && body.tools.length > 0
  if (!hasTools) {
    delete body.tools
    delete body.tool_choice
    delete body.parallel_tool_calls
  }

  if (typeof body.prompt_cache_key === "string" && body.prompt_cache_key) {
    body.prompt_cache_key = await fitCodexPromptCacheKey(body.prompt_cache_key)
  } else {
    delete body.prompt_cache_key
  }

  return stripRejectedCodexFields(body)
}

/**
 * Native path transport: same identity headers, relay, and error handling
 * as `chatCompletions`, but the upstream Responses SSE is returned as-is
 * (stream) or collected into its terminal `response` object (non-stream).
 * The reasoning replay cache is deliberately not involved — a Responses
 * client echoes its own reasoning items (docs/providers.md § Codex).
 */
async function codexNativeResponses(
  env: Env,
  acc: AcquiredAccount,
  chatgptAccountId: string,
  req: ChatCompletionRequest,
  extras: Parameters<ProviderAdapter["chatCompletions"]>[3],
  reasoning: { effort: string; summary: "auto" } | undefined,
): Promise<Response> {
  const body = await buildCodexNativeRequestBody(req.responsesBody!, {
    upstreamModel: req.upstreamModel,
    reasoning,
  })
  const headers: Record<string, string> = {
    authorization: `Bearer ${acc.credential.access_token}`,
    "user-agent": CODEX_USER_AGENT,
    originator: CODEX_ORIGINATOR,
    connection: "Keep-Alive",
    session_id: await codexSessionIdHeader(req),
    "content-type": "application/json",
    accept: "text/event-stream",
  }
  if (chatgptAccountId) headers["chatgpt-account-id"] = chatgptAccountId

  const { codexUpstream, relayFetch } = await import("./codex_relay")
  const upstream = codexUpstream(env)
  const res = await relayFetch(upstream, `${upstream.base}/codex/responses`, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
    signal: extras?.signal,
  })

  if (!res.ok) {
    const t = await res.text()
    return new Response(t, {
      status: res.status,
      headers: { "content-type": res.headers.get("content-type") || "application/json" },
    })
  }
  if (!res.body) return res

  if (req.stream) {
    return new Response(res.body, {
      status: 200,
      headers: {
        "content-type": "text/event-stream; charset=utf-8",
        "cache-control": "no-cache",
      },
    })
  }

  const { collectResponsesSse } = await import("../proxy/responses_openai")
  const collected = await collectResponsesSse(res.body)
  if ("error" in collected) return Response.json(collected, { status: 502 })
  return Response.json(collected.response)
}
