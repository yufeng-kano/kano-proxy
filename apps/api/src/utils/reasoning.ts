import type { ProviderId } from "../env"

export type ReasoningEffort =
  | "none"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "max"

const ALL: ReasoningEffort[] = ["none", "low", "medium", "high", "xhigh", "max"]

export function parseReasoningEffort(v: unknown): ReasoningEffort | undefined | "invalid" {
  if (v === undefined || v === null || v === "") return undefined
  if (typeof v !== "string") return "invalid"
  const s = v.toLowerCase() as ReasoningEffort
  return ALL.includes(s) ? s : "invalid"
}

/** Map client reasoning_effort to provider-specific payload fragments. */
export function mapReasoning(
  provider: ProviderId,
  effort: ReasoningEffort | undefined,
): {
  // OpenAI-compat body patch
  reasoning_effort?: string
  // Codex / Responses
  reasoning?: { effort: string; summary: "auto" }
  // Claude Messages
  output_config?: { effort: string }
  thinking?: { type: "disabled" }
  error?: string
} {
  if (effort === undefined) return {}

  if (provider === "grok") {
    if (effort === "max") return { error: "grok does not support reasoning_effort=max" }
    return { reasoning_effort: effort }
  }

  if (provider === "codex") {
    if (effort === "none") return {}
    if (effort === "max") return { error: "codex does not support reasoning_effort=max" }
    return { reasoning: { effort, summary: "auto" } }
  }

  // claude-code: effort-only public API; map none → disabled thinking + low effort
  if (effort === "none") {
    return { thinking: { type: "disabled" }, output_config: { effort: "low" } }
  }
  return { output_config: { effort } }
}
