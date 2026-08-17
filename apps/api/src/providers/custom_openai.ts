import type { CustomProviderRow } from "../db/custom_providers"
import { remapUnsupportedEffortBody } from "./custom_openai_reasoning"
import type { ProviderAdapter } from "./types"

const DEFAULT_ANTHROPIC_VERSION = "2023-06-01"

/**
 * BYO OpenAI-compatible endpoint. Near-passthrough: the outgoing body is the
 * client's own OpenAI Chat Completions body (or its Anthropic→OpenAI
 * conversion) with only `model` rewritten to the bare upstream id —
 * `temperature`, `reasoning_effort`, `response_format`, etc. all ride along
 * unmodified on the first send. Always also sets
 * `stream_options.include_usage: true` (stream and non-stream; TabbyAPI-class
 * upstreams omit `usage` without it). Client-supplied `stream_options` other
 * keys are kept. A recognized unsupported-effort HTTP 400 may rewrite only
 * `reasoning_effort` and POST once more on the same account. This
 * deliberately diverges from the built-in adapters, which strip `temperature`
 * and clamp `reasoning_effort` to a provider ceiling. No `messages()` — the
 * `/anthropic` surface reaches this adapter through the existing
 * Anthropic→OpenAI conversion path (`dispatchAnthropicViaOpenAI`), same as
 * grok/codex. `countTokens()` is added below, only when the row has a
 * `count_tokens_url` — with the field unset the method stays absent, which
 * is exactly what keeps `/anthropic/v1/messages/count_tokens` returning its
 * existing `400` for this format.
 */
export function createCustomOpenAIAdapter(row: CustomProviderRow): ProviderAdapter {
  const base = row.base_url

  const adapter: ProviderAdapter = {
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
      // TabbyAPI reports usage only when this flag is set, for stream and
      // non-stream alike. Merge so a client stream_options object is not
      // wholesale overwritten; force include_usage even if they sent false.
      const clientStreamOptions =
        req.rawBody &&
        typeof req.rawBody === "object" &&
        req.rawBody.stream_options &&
        typeof req.rawBody.stream_options === "object" &&
        !Array.isArray(req.rawBody.stream_options)
          ? (req.rawBody.stream_options as Record<string, unknown>)
          : {}
      const upstreamBody = {
        ...req.rawBody,
        model: req.upstreamModel,
        stream_options: { ...clientStreamOptions, include_usage: true },
      }
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

  // The operator's pointer to a real Anthropic-shaped count_tokens endpoint —
  // posted to verbatim, nothing appended. Same key as chatCompletions, sent
  // both ways (Bearer + x-api-key) since the target speaks Anthropic while
  // the key was entered as an OpenAI key. No effort-remap retry here — that
  // recovery is chatCompletions()-only.
  if (row.count_tokens_url) {
    const countTokensUrl = row.count_tokens_url
    adapter.countTokens = async (_env, account, body, headers, extras) => {
      const raw = typeof body === "object" && body ? { ...(body as Record<string, unknown>) } : {}
      const h: Record<string, string> = {
        authorization: `Bearer ${account.credential.access_token}`,
        "x-api-key": account.credential.access_token,
        "content-type": "application/json",
        "anthropic-version": headers.get("anthropic-version") || DEFAULT_ANTHROPIC_VERSION,
      }
      const beta = headers.get("anthropic-beta")
      if (beta) h["anthropic-beta"] = beta
      return fetch(countTokensUrl, {
        method: "POST",
        headers: h,
        body: JSON.stringify(raw),
        signal: extras?.signal,
      })
    }
  }

  return adapter
}

function isEventStream(res: Response): boolean {
  const ct = res.headers.get("content-type") || ""
  return ct.includes("text/event-stream")
}
