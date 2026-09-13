/** Anthropic-surface protocol details for dispatch (docs/api.md "In-stream errors", "Errors"). */
import { createAnthropicSseUsageSniffer, fromAnthropicUsage } from "../logging/usage_capture"
import type { Wire } from "./wire"

function frame(message: string, type: string): Uint8Array {
  return new TextEncoder().encode(
    `event: error\ndata: ${JSON.stringify({ type: "error", error: { type, message } })}\n\n`,
  )
}

function errorTypeFromBody(text: string): string {
  try {
    const j = JSON.parse(text) as { error?: { type?: string }; type?: string }
    if (typeof j.error?.type === "string" && j.error.type) return j.error.type
    if (j.type === "error" && typeof j.error === "object") return "api_error"
  } catch {
    /* not JSON */
  }
  return "api_error"
}

export const anthropicWire: Wire = {
  /** Exact text from docs/api.md "Keepalive and idle timeout". */
  stallFrame: new TextEncoder().encode(
    'event: error\ndata: {"type":"error","error":{"type":"overloaded_error","message":"upstream stalled: no data received for 120s"}}\n\n',
  ),
  noAccountFrame: (provider) => frame(`No usable ${provider} account`, "invalid_request_error"),
  unavailableFrame: () => frame("upstream_unavailable", "api_error"),
  upstreamErrorFrame: () => frame("upstream error", "api_error"),
  requestTooLargeFrame: (message) => frame(message, "invalid_request_error"),
  upstreamErrorFrameFromBody: (message, text) => frame(message, errorTypeFromBody(text)),
  noAccountBody: (provider) => ({
    type: "error",
    error: { type: "invalid_request_error", message: `No usable ${provider} account` },
  }),
  unavailableBody: () => ({ type: "error", error: { type: "api_error", message: "upstream_unavailable" } }),
  upstreamErrorBody: () => ({ type: "error", error: { type: "api_error", message: "upstream error" } }),
  requestTooLargeBody: (message) => ({
    type: "error",
    error: { type: "invalid_request_error", message },
  }),
  nonStreamResponse: "as_received",
  createUsageSniffer: createAnthropicSseUsageSniffer,
  parseUsage: fromAnthropicUsage,
}
