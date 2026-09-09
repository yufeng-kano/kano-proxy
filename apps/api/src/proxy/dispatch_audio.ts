/**
 * `/openai/v1/audio/transcriptions` dispatch (docs/api.md "Audio"): the
 * shared candidate walk via the non-stream transport, with audio's own
 * delivery — the response is passed through with every upstream header
 * and status, usage is tapped from a bounded prefix of a JSON body, and a
 * non-2xx pass-through is logged as `upstream_error`.
 */
import type { CustomProviderRow } from "../db/custom_providers"
import type { Env } from "../env"
import type { ProviderAdapter } from "../providers/types"
import type { RoutingCandidate } from "../routing/types"
import { logRequest } from "../logging/request_log"
import { fromOpenAIUsage, type NormalizedUsage } from "../logging/usage_capture"
import {
  DEFAULT_IDLE_TIMEOUT_MS,
  canonicalModelId,
  dispatchNonStream,
  isEventStream,
  passthroughStreamHeaders,
  streamCloseErrorCode,
  usageFields,
} from "./dispatch"
import type { WaitUntil } from "./dispatch_walk"
import { streamWithKeepalive, type StreamCloseReason } from "./sse"
import { openaiWire } from "./wire_openai"

export async function dispatchAudioTranscriptions(
  env: Env,
  opts: {
    userId: string
    apiKeyId: string | null
    provider: string
    adapter?: ProviderAdapter
    formData: FormData
    rawModel: string
    upstreamModel: string
    waitUntil: WaitUntil
    idleTimeoutMs?: number
    groupName?: string
    pinnedAccountId?: string
    candidates?: RoutingCandidate[]
    strategy?: string
    isBuiltin?: boolean
    customProvider?: CustomProviderRow
  },
): Promise<Response> {
  const idleTimeoutMs = opts.idleTimeoutMs ?? DEFAULT_IDLE_TIMEOUT_MS
  return dispatchNonStream(
    env,
    {
      userId: opts.userId,
      apiKeyId: opts.apiKeyId,
      groupName: opts.groupName,
      waitUntil: opts.waitUntil,
      idleTimeoutMs,
      requested: { provider: opts.provider, model: canonicalModelId(opts.provider, opts.upstreamModel) },
      source: opts,
      upstreamModel: opts.upstreamModel,
      callFor: (candidate) => {
        const call = candidate.adapter.audioTranscriptions
        if (!call) return null
        return (acquired, signal) =>
          call(env, acquired, opts.formData, opts.rawModel, candidate.upstreamModel, { signal })
      },
      wire: openaiWire,
      captureUsage: true,
    },
    async (candidate, res, latencyMs) => {
      const errorCode = res.ok ? null : "upstream_error"
      const log = (usage: NormalizedUsage | null, code: string | null) =>
        opts.waitUntil(
          logRequest(env, {
            userId: opts.userId,
            apiKeyId: opts.apiKeyId,
            provider: candidate.provider,
            model: canonicalModelId(candidate.provider, candidate.upstreamModel),
            accountId: candidate.account.id,
            statusCode: res.status,
            latencyMs,
            errorCode: code,
            upstreamStatus: res.status,
            groupName: opts.groupName ?? null,
            ...usageFields(usage),
          }),
        )

      if (res.body && isEventStream(res)) {
        const sniffer = openaiWire.createUsageSniffer()
        const streamBody = streamWithKeepalive(res.body, undefined, {
          tap: (chunk) => sniffer.feed(chunk),
          idleTimeoutMs,
          stallFrame: openaiWire.stallFrame,
          onClose: (reason) => log(sniffer.finish(), errorCode ?? streamCloseErrorCode(reason, sniffer.complete())),
        })
        return new Response(streamBody, { status: res.status, headers: passthroughStreamHeaders(res.headers) })
      }

      const isJson = (res.headers.get("content-type") || "").includes("application/json")
      const onFinish = (usage: NormalizedUsage | null, reason: StreamCloseReason) =>
        log(usage, reason === "cancel" ? "client_abort" : reason === "error" ? "upstream_error" : errorCode)

      if (!res.body) {
        onFinish(null, "done")
        return res
      }
      return new Response(streamWithUsageTap(res.body, isJson, onFinish), {
        status: res.status,
        statusText: res.statusText,
        headers: res.headers,
      })
    },
  )
}

/** Pass the body through untouched while buffering at most 256 KiB of a JSON body to read `usage` from once it ends. */
function streamWithUsageTap(
  upstream: ReadableStream<Uint8Array>,
  isJson: boolean,
  onFinish: (usage: NormalizedUsage | null, reason: StreamCloseReason) => void,
): ReadableStream<Uint8Array> {
  let finished = false
  let boundedBuf = ""
  let boundedLen = 0
  const MAX_TAP = 256 * 1024
  const decoder = new TextDecoder()
  const reader = upstream.getReader()

  const finishOnce = (reason: StreamCloseReason) => {
    if (finished) return
    finished = true
    let usage: NormalizedUsage | null = null
    if (isJson && boundedBuf) {
      try {
        const j = JSON.parse(boundedBuf) as { usage?: Record<string, unknown> }
        usage = fromOpenAIUsage(j.usage)
      } catch {
        /* malformed or truncated JSON */
      }
    }
    onFinish(usage, reason)
  }

  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        const { done, value } = await reader.read()
        if (done) {
          controller.close()
          finishOnce("done")
          return
        }
        if (value) {
          controller.enqueue(value)
          if (isJson && boundedLen < MAX_TAP) {
            try {
              boundedBuf += decoder.decode(value, { stream: true })
              boundedLen += value.byteLength
            } catch {
              /* */
            }
          }
        }
      } catch (err) {
        controller.error(err)
        finishOnce("error")
      }
    },
    async cancel(reason) {
      try {
        await reader.cancel(reason)
      } catch {
        /* */
      }
      finishOnce("cancel")
    },
  })
}
