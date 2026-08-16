import type { ProviderId } from "../env"

export type ReasoningEffort =
  | "none"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "max"

export const REASONING_EFFORTS: readonly ReasoningEffort[] = [
  "none",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
]

export function parseReasoningEffort(v: unknown): ReasoningEffort | undefined | "invalid" {
  if (v === undefined || v === null || v === "") return undefined
  if (typeof v !== "string") return "invalid"
  const s = v.toLowerCase() as ReasoningEffort
  return REASONING_EFFORTS.includes(s) ? s : "invalid"
}

/**
 * Closest allowed ladder token to `rejected`. Equal distance prefers the
 * higher token. Empty `allowed` → undefined. Identity when `rejected` is
 * already in `allowed`.
 */
export function nearestReasoningEffort(
  rejected: ReasoningEffort,
  allowed: readonly ReasoningEffort[],
): ReasoningEffort | undefined {
  if (allowed.length === 0) return undefined
  if (allowed.includes(rejected)) return rejected

  const rejectedIdx = REASONING_EFFORTS.indexOf(rejected)
  let best: ReasoningEffort | undefined
  let bestDist = Infinity
  for (const token of allowed) {
    const idx = REASONING_EFFORTS.indexOf(token)
    const dist = Math.abs(idx - rejectedIdx)
    if (best === undefined || dist < bestDist) {
      best = token
      bestDist = dist
    } else if (dist === bestDist && idx > REASONING_EFFORTS.indexOf(best)) {
      best = token
    }
  }
  return best
}

/**
 * Highest effort each provider's API accepts; efforts above it clamp down
 * instead of erroring. Verified 2026-08-02: xAI tops out at `xhigh`
 * (docs.x.ai: grok-4.5 high / grok-4.20-multi-agent xhigh; live grok-4.5
 * accepts xhigh) and codex Responses models top out at `xhigh` (`max` is
 * only a non-codex GPT-5.6 value). See docs/api.md.
 */
const CEILING: Record<ProviderId, ReasoningEffort> = {
  grok: "xhigh",
  codex: "xhigh",
  "claude-code": "max",
}

function clampToCeiling(provider: ProviderId, effort: ReasoningEffort): ReasoningEffort {
  const cap = CEILING[provider]
  return REASONING_EFFORTS.indexOf(effort) > REASONING_EFFORTS.indexOf(cap) ? cap : effort
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
} {
  if (effort === undefined) return {}
  const capped = clampToCeiling(provider, effort)

  if (provider === "grok") {
    return { reasoning_effort: capped }
  }

  if (provider === "codex") {
    if (capped === "none") return {}
    return { reasoning: { effort: capped, summary: "auto" } }
  }

  // claude-code: effort-only public API; map none → disabled thinking + low effort
  if (capped === "none") {
    return { thinking: { type: "disabled" }, output_config: { effort: "low" } }
  }
  return { output_config: { effort: capped } }
}
