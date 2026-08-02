/**
 * Session-scoped KV cache for Grok Responses encrypted reasoning replay.
 *
 * Claude Code / CC Switch may omit thinking.signature on later turns; xAI
 * still needs the prior turn's encrypted_content in Responses input.
 *
 * Isolation: SHA-256(api_key_id + model + session) — never shared across
 * callers or models. KV is eventually consistent; a put scheduled via
 * waitUntil may not be visible on the very next turn under load — see
 * docs/providers.md.
 */

import type { Env } from "../env"
import { isValidGrokEncryptedContent } from "./grok_encrypted_content"

export const GROK_REASONING_REPLAY_TTL_SECONDS = 3600

export type GrokReasoningReplayEntry = {
  /** Opaque xAI encrypted_content (also exposed as Anthropic thinking.signature). */
  encrypted_content: string
  /** SHA-256 hex of trailing assistant plaintext — match before reinject. */
  assistant_text_hash: string
}

export function grokReasoningReplaySessionKey(affinity?: {
  convId?: string
  sessionId?: string
}): string | null {
  const conv = affinity?.convId?.trim()
  if (conv) return conv
  const session = affinity?.sessionId?.trim()
  if (session) return session
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
  return `grok-reasoning-replay:v2:${hex(new Uint8Array(digest))}`
}

function hex(buf: Uint8Array): string {
  let out = ""
  for (let i = 0; i < buf.length; i++) {
    out += buf[i]!.toString(16).padStart(2, "0")
  }
  return out
}

export async function readGrokReasoningReplay(
  env: Env,
  apiKeyId: string,
  model: string,
  sessionKey: string,
): Promise<GrokReasoningReplayEntry | null> {
  if (!apiKeyId || !model || !sessionKey) return null
  try {
    const key = await scopedCacheKey(apiKeyId, model, sessionKey)
    const raw = await env.CACHE.get(key, "json")
    if (!raw || typeof raw !== "object") return null
    const entry = raw as GrokReasoningReplayEntry
    if (
      typeof entry.encrypted_content !== "string" ||
      !isValidGrokEncryptedContent(entry.encrypted_content) ||
      typeof entry.assistant_text_hash !== "string" ||
      !entry.assistant_text_hash
    ) {
      return null
    }
    return entry
  } catch {
    return null
  }
}

export async function writeGrokReasoningReplay(
  env: Env,
  apiKeyId: string,
  model: string,
  sessionKey: string,
  entry: GrokReasoningReplayEntry,
): Promise<void> {
  if (!apiKeyId || !model || !sessionKey) return
  if (!isValidGrokEncryptedContent(entry.encrypted_content)) return
  if (!entry.assistant_text_hash) return
  try {
    const key = await scopedCacheKey(apiKeyId, model, sessionKey)
    await env.CACHE.put(key, JSON.stringify(entry), {
      expirationTtl: GROK_REASONING_REPLAY_TTL_SECONDS,
    })
  } catch {
    /* cache write must not break the request */
  }
}

export async function deleteGrokReasoningReplay(
  env: Env,
  apiKeyId: string,
  model: string,
  sessionKey: string,
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
export async function grokReasoningReplayCacheKeyForTest(
  apiKeyId: string,
  model: string,
  sessionKey: string,
): Promise<string> {
  return scopedCacheKey(apiKeyId, model, sessionKey)
}
