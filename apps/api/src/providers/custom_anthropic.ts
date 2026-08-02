import type { AcquiredAccount } from "../pool/acquire"
import type { CustomProviderRow } from "../db/custom_providers"
import { anthropicToOpenAIResponse, openaiToAnthropicMessages } from "../proxy/openai_anthropic"
import type { ProviderAdapter } from "./types"

const DEFAULT_ANTHROPIC_VERSION = "2023-06-01"

/**
 * BYO Anthropic-compatible endpoint. `messages()` / `countTokens()` are a
 * native passthrough (mirrors claude-code's `forwardToAnthropic`) MINUS every
 * Claude-Code-OAuth specific: no required-system prepend, no auto-added
 * `effort-2025-11-24` beta, no fixed base betas — the `anthropic-beta` header
 * is forwarded verbatim from the client (or omitted) rather than resolved.
 * `chatCompletions()` builds a request via the shared OpenAI↔Anthropic
 * converters for the `/openai/v1` surface; `reasoning_effort` is dropped
 * there on purpose (the native surface gives full `thinking` control).
 */
export function createCustomAnthropicAdapter(row: CustomProviderRow): ProviderAdapter {
  const base = row.base_url

  async function forwardNative(
    url: string,
    account: AcquiredAccount,
    body: unknown,
    headers: Headers,
  ): Promise<Response> {
    const raw = typeof body === "object" && body ? { ...(body as Record<string, unknown>) } : {}
    const h: Record<string, string> = {
      "x-api-key": account.credential.access_token,
      "content-type": "application/json",
      "anthropic-version": headers.get("anthropic-version") || DEFAULT_ANTHROPIC_VERSION,
    }
    const beta = headers.get("anthropic-beta")
    if (beta) h["anthropic-beta"] = beta
    return fetch(url, { method: "POST", headers: h, body: JSON.stringify(raw) })
  }

  return {
    id: row.slug,

    async messages(_env, account, body, headers) {
      return forwardNative(`${base}/v1/messages`, account, body, headers)
    },

    async countTokens(_env, account, body, headers) {
      return forwardNative(`${base}/v1/messages/count_tokens`, account, body, headers)
    },

    async chatCompletions(_env, account, req) {
      const anthropicBody = openaiToAnthropicMessages({
        model: req.upstreamModel,
        messages: req.messages,
        max_tokens: req.max_tokens ?? 4096,
        stream: req.stream,
        tools: req.tools,
        tool_choice: req.tool_choice,
        response_format: req.response_format,
        stop: req.stop,
        // reasoning_effort intentionally dropped on this surface — no
        // thinking/output_config mapped from it for custom upstreams.
      })
      const res = await fetch(`${base}/v1/messages`, {
        method: "POST",
        headers: {
          "x-api-key": account.credential.access_token,
          "content-type": "application/json",
          "anthropic-version": DEFAULT_ANTHROPIC_VERSION,
        },
        body: JSON.stringify(anthropicBody),
      })

      if (req.stream) {
        if (!res.ok || !res.body) return res
        const { anthropicSseToOpenAIStream } = await import("../proxy/openai_anthropic")
        return new Response(anthropicSseToOpenAIStream(res.body, req.rawModel), {
          status: res.status,
          headers: {
            "content-type": "text/event-stream; charset=utf-8",
            "cache-control": "no-cache",
          },
        })
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
        return Response.json(anthropicToOpenAIResponse(json, req.rawModel))
      } catch {
        return new Response(text, { status: res.status })
      }
    },

    async listModels(_env, account) {
      try {
        const res = await fetch(`${base}/v1/models`, {
          headers: {
            "x-api-key": account.credential.access_token,
            "anthropic-version": DEFAULT_ANTHROPIC_VERSION,
          },
        })
        if (!res.ok) return { models: [], error: `models ${res.status}` }
        const json = (await res.json()) as {
          data?: Array<{ id?: string; display_name?: string }>
        }
        const models = (json.data ?? [])
          .filter((m) => typeof m.id === "string" && m.id)
          .map((m) => ({ id: m.id as string, display_name: m.display_name ?? null }))
        return { models, error: null }
      } catch (e) {
        return { models: [], error: e instanceof Error ? e.message : "models fetch failed" }
      }
    },
  }
}
