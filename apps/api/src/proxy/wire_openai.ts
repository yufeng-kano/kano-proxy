/** OpenAI-surface protocol details for dispatch (docs/api.md "In-stream errors", "Errors"). */
import { createOpenAISseUsageSniffer, fromOpenAIUsage } from "../logging/usage_capture"
import type { Wire } from "./wire"

function frame(message: string, type: string, code: string): Uint8Array {
  return new TextEncoder().encode(`data: ${JSON.stringify({ error: { message, type, code } })}\n\n`)
}

function errorTypeFromBody(text: string): string {
  try {
    const j = JSON.parse(text) as { error?: { type?: string } }
    if (typeof j.error?.type === "string" && j.error.type) return j.error.type
  } catch {
    /* not JSON */
  }
  return "api_error"
}

export const openaiWire: Wire = {
  /** Exact text from docs/api.md "Keepalive and idle timeout". */
  stallFrame: new TextEncoder().encode(
    'data: {"error":{"message":"upstream stalled: no data received for 120s","type":"api_error","code":"upstream_stall"}}\n\n',
  ),
  noAccountFrame: (provider) =>
    frame(`No usable ${provider} account for this user`, "invalid_request_error", "no_upstream_account"),
  unavailableFrame: () => frame("All upstream accounts unavailable", "api_error", "upstream_unavailable"),
  upstreamErrorFrame: () => frame("upstream error", "api_error", "upstream_error"),
  upstreamErrorFrameFromBody: (message, text) => frame(message, errorTypeFromBody(text), "upstream_error"),
  noAccountBody: (provider) => ({
    error: {
      message: `No usable ${provider} account for this user`,
      type: "invalid_request_error",
      code: "no_upstream_account",
    },
  }),
  unavailableBody: () => ({ error: { message: "All upstream accounts unavailable", code: "upstream_unavailable" } }),
  upstreamErrorBody: () => ({ error: { message: "upstream error", code: "upstream_error" } }),
  createUsageSniffer: createOpenAISseUsageSniffer,
  parseUsage: fromOpenAIUsage,
}
