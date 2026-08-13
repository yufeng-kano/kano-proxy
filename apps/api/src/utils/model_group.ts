/** Validation and limits for user-defined model groups (docs/providers.md § Model groups). */

import { splitModelId } from "./model"

export const MAX_MODEL_GROUPS_PER_USER = 50
export const MAX_TARGETS_PER_GROUP = 20
export const MAX_GROUP_NAME_LENGTH = 128

/**
 * Trimmed, 1-128 chars, no whitespace, no "/" — the missing slash is the
 * namespace isolation from `provider/model` ids (`splitModelId` requires a
 * slash), so a bare name can never collide with a real model id.
 */
export function validateGroupName(name: string): string | null {
  if (!name || name.length > MAX_GROUP_NAME_LENGTH) {
    return `name must be 1-${MAX_GROUP_NAME_LENGTH} characters`
  }
  if (/\s/.test(name)) return "name must not contain whitespace"
  if (name.includes("/")) return "name must not contain '/'"
  return null
}

export type GroupTargetsValidation = { ok: true; targets: string[] } | { ok: false; error: string }

/**
 * `resolvePrefix` decides whether a target's `provider/model` prefix is a
 * valid builtin `ProviderId` or one of the caller's own custom slugs — DB
 * access lives in the route, this helper stays pure/sync.
 */
export function validateGroupTargets(
  targets: unknown,
  resolvePrefix: (prefix: string) => boolean,
): GroupTargetsValidation {
  if (!Array.isArray(targets) || targets.length === 0) {
    return { ok: false, error: "targets must be a non-empty array" }
  }
  if (targets.length > MAX_TARGETS_PER_GROUP) {
    return { ok: false, error: `targets must have at most ${MAX_TARGETS_PER_GROUP} entries` }
  }
  const out: string[] = []
  const seen = new Set<string>()
  for (const t of targets) {
    if (typeof t !== "string") return { ok: false, error: "targets entries must be strings" }
    const trimmed = t.trim()
    const split = splitModelId(trimmed)
    if (!split) {
      return { ok: false, error: `target "${trimmed}" must be provider/model` }
    }
    // A bare name (no slash) is rejected above by splitModelId already, but
    // spell it out: a group can never target another group — no nesting, no
    // cycles, structurally.
    if (!resolvePrefix(split.prefix)) {
      return { ok: false, error: `target "${trimmed}" has an unknown provider "${split.prefix}"` }
    }
    if (seen.has(trimmed)) {
      return { ok: false, error: `duplicate target "${trimmed}"` }
    }
    seen.add(trimmed)
    out.push(trimmed)
  }
  return { ok: true, targets: out }
}
