/**
 * Everything about a dispatch surface that depends on the client protocol
 * (docs/project-structure.md § Boundaries). The transports in `dispatch.ts`
 * call these and never branch on OpenAI vs Anthropic themselves; the two
 * implementations are `wire_openai.ts` and `wire_anthropic.ts`.
 */
import type { NormalizedUsage, UsageSniffer } from "../logging/usage_capture"

export type Wire = {
  /** Terminal frame emitted after 120s of upstream silence while piping (docs/api.md "Keepalive and idle timeout"). */
  stallFrame: Uint8Array
  /** Eager-commit terminal error frames (docs/api.md "In-stream errors"). */
  noAccountFrame: (provider: string) => Uint8Array
  unavailableFrame: () => Uint8Array
  upstreamErrorFrame: () => Uint8Array
  /** Upstream non-2xx after failover: `message` already extracted from `text`, the error type is read from `text` in the surface's own envelope shape. */
  upstreamErrorFrameFromBody: (message: string, text: string) => Uint8Array
  /** Non-stream JSON error envelopes (docs/api.md "Errors"). */
  noAccountBody: (provider: string) => Record<string, unknown>
  unavailableBody: () => Record<string, unknown>
  upstreamErrorBody: () => Record<string, unknown>
  /** Token usage from a piped SSE body / a non-stream JSON body's `usage` (docs/logging.md "Token usage capture"). */
  createUsageSniffer: () => UsageSniffer
  parseUsage: (usage: Record<string, unknown> | null | undefined) => NormalizedUsage
}
