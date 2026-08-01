import type { Env } from "../env"
import type { AcquiredAccount } from "../pool/acquire"
import { saveCredential } from "../pool/acquire"
import { mapReasoning } from "../utils/reasoning"
import type { ChatCompletionRequest, ProviderAdapter, UsageWindow } from "./types"

const TOKEN_URL = "https://auth.openai.com/oauth/token"
const CODEX_BASE = "https://chatgpt.com/backend-api"
const DEFAULT_CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann"

function clientId(env: Env): string {
  return env.CODEX_OAUTH_CLIENT_ID || DEFAULT_CLIENT_ID
}

async function refreshCodex(env: Env, account: AcquiredAccount): Promise<AcquiredAccount> {
  const { credential } = account
  if (!credential.refresh_token) return account
  const exp = credential.expires_at ? Date.parse(credential.expires_at) : 0
  if (exp && exp - 60_000 > Date.now()) return account

  const res = await fetch(TOKEN_URL, {
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

  /** ChatGPT Codex OAuth has no public models list endpoint (same as lincy). */
  async listModels(_env, _account) {
    return {
      models: [],
      error: "Codex has no upstream models list API",
    }
  },

  async chatCompletions(env, account, req) {
    const acc = await refreshCodex(env, account)
    const mapped = mapReasoning("codex", req.reasoning_effort)
    if (mapped.error) {
      return Response.json(
        { error: { message: mapped.error, code: "invalid_reasoning" } },
        { status: 400 },
      )
    }
    const chatgptAccountId =
      acc.credential.account_id || accountIdFromJwt(acc.credential.access_token) || ""

    const input = openaiMessagesToCodexInput(req.messages)
    const body: Record<string, unknown> = {
      model: req.upstreamModel,
      input,
      stream: true, // codex backend is SSE-oriented
    }
    if (mapped.reasoning) body.reasoning = mapped.reasoning
    if (req.tools) body.tools = mapTools(req.tools)
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

    const res = await fetch(`${CODEX_BASE}/codex/responses`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${acc.credential.access_token}`,
        "chatgpt-account-id": chatgptAccountId,
        "OpenAI-Beta": "responses=experimental",
        originator: "codex_cli_rs",
        "content-type": "application/json",
        accept: "text/event-stream",
      },
      body: JSON.stringify(body),
    })

    if (!res.ok) {
      const t = await res.text()
      return new Response(t, {
        status: res.status,
        headers: { "content-type": res.headers.get("content-type") || "application/json" },
      })
    }

    if (req.stream) {
      if (!res.body) return res
      const { codexSseToOpenAIStream } = await import("../proxy/codex_openai")
      return new Response(codexSseToOpenAIStream(res.body, req.upstreamModel), {
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
    const openai = await collectCodexSse(res.body, req.upstreamModel)
    return Response.json(openai)
  },

  async fetchUsage(env, account) {
    const acc = await refreshCodex(env, account)
    const chatgptAccountId =
      acc.credential.account_id || accountIdFromJwt(acc.credential.access_token) || ""
    const { fetchCodexUsageJson, windowsFromCodexPayload } = await import("./codex_usage")
    const result = await fetchCodexUsageJson(acc.credential.access_token, chatgptAccountId)

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

function openaiMessagesToCodexInput(messages: unknown[]): unknown[] {
  const input: unknown[] = []
  for (const m of messages as Array<Record<string, unknown>>) {
    const role = String(m.role ?? "")
    if (role === "system") {
      // folded into instructions by caller if needed; keep as message
      input.push({
        role: "user",
        content: [{ type: "input_text", text: `[system]\n${contentText(m.content)}` }],
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
      const toolCalls = m.tool_calls as Array<Record<string, unknown>> | undefined
      if (toolCalls?.length) {
        for (const tc of toolCalls) {
          const fn = tc.function as { name?: string; arguments?: string }
          input.push({
            type: "function_call",
            call_id: tc.id,
            name: fn?.name,
            arguments: fn?.arguments ?? "{}",
          })
        }
      }
      const text = contentText(m.content)
      if (text) {
        input.push({
          role: "assistant",
          content: [{ type: "output_text", text }],
        })
      }
      continue
    }
    if (role === "tool") {
      input.push({
        type: "function_call_output",
        call_id: m.tool_call_id,
        output: contentText(m.content),
      })
    }
  }
  return input
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
