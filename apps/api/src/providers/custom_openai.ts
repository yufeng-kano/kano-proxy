import type { CustomProviderRow } from "../db/custom_providers"
import { remapUnsupportedEffortBody } from "./custom_openai_reasoning"
import type { ProviderAdapter } from "./types"

/**
 * BYO OpenAI-compatible endpoint. Near-passthrough: the outgoing body is the
 * client's own OpenAI Chat Completions body (or its Anthropic→OpenAI
 * conversion) with only `model` rewritten to the bare upstream id —
 * `temperature`, `reasoning_effort`, `response_format`, etc. all ride along
 * unmodified on the first send. A recognized unsupported-effort HTTP 400 may
 * rewrite only `reasoning_effort` and POST once more on the same account.
 * This deliberately diverges from the built-in adapters, which strip
 * `temperature` and clamp `reasoning_effort` to a provider ceiling. No
 * `messages()` — the `/anthropic` surface reaches this adapter through the
 * existing Anthropic→OpenAI conversion path (`dispatchAnthropicViaOpenAI`),
 * same as grok/codex.
 */
export function createCustomOpenAIAdapter(row: CustomProviderRow): ProviderAdapter {
  const base = row.base_url

  return {
    id: row.slug,

    async chatCompletions(_env, account, req, extras) {
      const url = `${base}/chat/completions`
      const headers = {
        authorization: `Bearer ${account.credential.access_token}`,
        "content-type": "application/json",
      }
      const init = {
        method: "POST",
        headers,
        signal: extras?.signal,
      }
      const upstreamBody = { ...req.rawBody, model: req.upstreamModel }
      const res = await fetch(url, { ...init, body: JSON.stringify(upstreamBody) })
      if (res.ok || res.status !== 400 || isEventStream(res)) return res

      const text = await res.text()
      const remapped = remapUnsupportedEffortBody(upstreamBody, text)
      if (!remapped) {
        return new Response(text, {
          status: res.status,
          statusText: res.statusText,
          headers: res.headers,
        })
      }
      return fetch(url, { ...init, body: JSON.stringify(remapped) })
    },

    async listModels(_env, account) {
      try {
        const res = await fetch(`${base}/models`, {
          headers: { authorization: `Bearer ${account.credential.access_token}` },
        })
        if (!res.ok) return { models: [], error: `models ${res.status}` }
        const json = (await res.json()) as { data?: Array<{ id?: string }> }
        const models = (json.data ?? [])
          .filter((m) => typeof m.id === "string" && m.id)
          .map((m) => ({ id: m.id as string, display_name: null }))
        return { models, error: null }
      } catch (e) {
        return { models: [], error: e instanceof Error ? e.message : "models fetch failed" }
      }
    },
  }
}

function isEventStream(res: Response): boolean {
  const ct = res.headers.get("content-type") || ""
  return ct.includes("text/event-stream")
}
