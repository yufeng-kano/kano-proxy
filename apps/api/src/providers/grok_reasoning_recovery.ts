/**
 * Recover from xAI/cli-chat-proxy opaque-state decode rejections.
 *
 * Claude Code → Messages → Responses can surface either:
 *   - "Could not decode the compaction blob..."
 *   - "Could not decrypt the provided encrypted_content"
 * when a prior thinking.signature / KV replay / session sticky state is
 * foreign, truncated, or bound to a different upstream session. Shape checks
 * cannot prove decryptability — retry after stripping opaque items (and, if
 * needed, clearing affinity headers). Inspired by grok2api PR #721.
 */

const DECODE_FAILURE_MARKERS = [
  "could not decode the compaction blob",
  "could not decrypt the provided encrypted_content",
] as const

export function isGrokOpaqueDecodeFailure(bodyText: string): boolean {
  const lower = bodyText.toLowerCase()
  return DECODE_FAILURE_MARKERS.some((m) => lower.includes(m))
}

export type StripOpaqueResult = {
  body: Record<string, unknown>
  /** True when at least one reasoning.encrypted_content or compaction item was removed. */
  changed: boolean
}

/**
 * Remove opaque replay state from a Responses request body.
 * - reasoning: drop `encrypted_content`; drop the item entirely if nothing readable remains
 * - compaction: drop the whole item (ciphertext is the only payload)
 */
export function stripResponsesOpaqueState(
  body: Record<string, unknown>,
): StripOpaqueResult {
  const input = body.input
  if (!Array.isArray(input) || input.length === 0) {
    return { body, changed: false }
  }

  let changed = false
  const next: unknown[] = []
  for (const raw of input) {
    if (!raw || typeof raw !== "object") {
      next.push(raw)
      continue
    }
    const item = raw as Record<string, unknown>
    const type = typeof item.type === "string" ? item.type : ""

    if (type === "compaction") {
      changed = true
      continue
    }

    if (type !== "reasoning") {
      next.push(raw)
      continue
    }

    const encrypted = item.encrypted_content
    if (typeof encrypted !== "string" || !encrypted) {
      next.push(raw)
      continue
    }

    changed = true
    const cleaned: Record<string, unknown> = { ...item }
    delete cleaned.encrypted_content
    delete cleaned.id
    delete cleaned.status
    if (hasReadableReasoningContent(cleaned)) {
      next.push(cleaned)
    }
  }

  if (!changed) return { body, changed: false }
  return { body: { ...body, input: next }, changed: true }
}

/** Drop prompt_cache_key so a session-reset retry cannot reattach sticky cache. */
export function stripPromptCacheKey(
  body: Record<string, unknown>,
): Record<string, unknown> {
  if (!("prompt_cache_key" in body)) return body
  const next = { ...body }
  delete next.prompt_cache_key
  return next
}

export function affinityPresent(affinity?: {
  convId?: string
  sessionId?: string
  turnIdx?: string
}): boolean {
  return !!(
    affinity?.convId?.trim() ||
    affinity?.sessionId?.trim() ||
    affinity?.turnIdx?.trim()
  )
}

function hasReadableReasoningContent(item: Record<string, unknown>): boolean {
  for (const field of ["summary", "content"] as const) {
    const parts = item[field]
    if (!Array.isArray(parts)) continue
    for (const part of parts) {
      if (
        part &&
        typeof part === "object" &&
        typeof (part as { text?: unknown }).text === "string" &&
        (part as { text: string }).text.trim()
      ) {
        return true
      }
    }
  }
  return false
}
