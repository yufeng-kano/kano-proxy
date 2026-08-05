/** Session-scoped KV cache for Codex Responses reasoning replay. */

import type { Env } from "../env"

export const CODEX_REASONING_REPLAY_TTL_SECONDS = 3600
const CODEX_REASONING_REPLAY_MAX_BYTES = 256 * 1024

export type CodexReasoningReplayItem = {
  [key: string]: unknown
  type: string
}

export type CodexReasoningReplayEntry = {
  /** Ordered opaque Responses output items to prepend on the next turn. */
  items: CodexReasoningReplayItem[]
  /** SHA-256 hex of trailing assistant plaintext — match before reinject. */
  assistant_text_hash: string
}


export function codexReasoningReplaySessionKey(
  affinity?: {
    convId?: string
    sessionId?: string
  },
  promptCacheKey?: string,
): string | null {
  const conv = affinity?.convId?.trim()
  if (conv) return conv
  const session = affinity?.sessionId?.trim()
  if (session) return session
  // Same stability contract as the affinity headers: client-sent on
  // /openai/v1, metadata.user_id-derived on /anthropic (docs/providers.md).
  const key = promptCacheKey?.trim()
  if (key) return key
  return null
}

export async function hashAssistantText(text: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(text),
  )
  return hex(new Uint8Array(digest))
}

async function scopedCacheKey(
  apiKeyId: string,
  model: string,
  sessionKey: string,
): Promise<string> {
  const material = `${apiKeyId}\0${model}\0${sessionKey}`
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(material),
  )
  return `codex-reasoning-replay:v1:${hex(new Uint8Array(digest))}`
}

function hex(buf: Uint8Array): string {
  let out = ""
  for (let i = 0; i < buf.length; i++) {
    out += buf[i]!.toString(16).padStart(2, "0")
  }
  return out
}

function isReplayItem(value: unknown): value is CodexReasoningReplayItem {
  return (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    typeof (value as { type?: unknown }).type === "string"
  )
}

function isReplayEntry(value: unknown): value is CodexReasoningReplayEntry {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false
  const entry = value as {
    items?: unknown
    assistant_text_hash?: unknown
  }
  return (
    Array.isArray(entry.items) &&
    entry.items.every(isReplayItem) &&
    typeof entry.assistant_text_hash === "string" &&
    entry.assistant_text_hash.length > 0
  )
}

export async function readCodexReasoningReplay(
  env: Env,
  apiKeyId: string,
  model: string,
  sessionKey: string | null,
): Promise<CodexReasoningReplayEntry | null> {
  if (!apiKeyId || !model || !sessionKey) return null
  try {
    const key = await scopedCacheKey(apiKeyId, model, sessionKey)
    const raw = await env.CACHE.get(key, "json")
    const parsed = typeof raw === "string" ? JSON.parse(raw) : raw
    if (!isReplayEntry(parsed)) return null
    return parsed
  } catch {
    return null
  }
}

export async function writeCodexReasoningReplay(
  env: Env,
  apiKeyId: string,
  model: string,
  sessionKey: string | null,
  entry: CodexReasoningReplayEntry,
): Promise<void> {
  if (!apiKeyId || !model || !sessionKey || !isReplayEntry(entry)) return
  try {
    const serialized = JSON.stringify(entry)
    if (new TextEncoder().encode(serialized).byteLength > CODEX_REASONING_REPLAY_MAX_BYTES) {
      return
    }
    const key = await scopedCacheKey(apiKeyId, model, sessionKey)
    await env.CACHE.put(key, serialized, {
      expirationTtl: CODEX_REASONING_REPLAY_TTL_SECONDS,
    })
  } catch {
    /* cache write must not break the request */
  }
}

export async function deleteCodexReasoningReplay(
  env: Env,
  apiKeyId: string,
  model: string,
  sessionKey: string | null,
): Promise<void> {
  if (!apiKeyId || !model || !sessionKey) return
  try {
    const key = await scopedCacheKey(apiKeyId, model, sessionKey)
    await env.CACHE.delete(key)
  } catch {
    /* */
  }
}

/** Exported for tests — same material as the private scoped key. */
export async function codexReasoningReplayCacheKeyForTest(
  apiKeyId: string,
  model: string,
  sessionKey: string,
): Promise<string> {
  return scopedCacheKey(apiKeyId, model, sessionKey)
}
