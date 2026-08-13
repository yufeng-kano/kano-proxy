/** Validation and limits for user-defined model groups (docs/providers.md § Model groups). */

import type { GroupTarget } from "../db/model_groups"
import { splitModelId } from "./model"

export const MAX_MODEL_GROUPS_PER_USER = 50
export const MAX_TARGETS_PER_GROUP = 20
export const MAX_ALIASES_PER_GROUP = 10
export const MAX_ALIAS_LENGTH = 128
export const MAX_DISPLAY_NAME_LENGTH = 64

/**
 * One callable alias: trimmed, 1-128 chars, no whitespace, no "/" — the
 * missing slash is the namespace isolation from `provider/model` ids
 * (`splitModelId` requires a slash), so an alias can never collide with a
 * real model id.
 */
export function validateAlias(alias: string): string | null {
  if (!alias || alias.length > MAX_ALIAS_LENGTH) {
    return `alias must be 1-${MAX_ALIAS_LENGTH} characters`
  }
  if (/\s/.test(alias)) return "alias must not contain whitespace"
  if (alias.includes("/")) return "alias must not contain '/'"
  return null
}

/**
 * A group's display name: trimmed, 1-64 chars, free text (spaces fine — a
 * label, never a callable id; the callable ids are `aliases`).
 */
export function validateDisplayName(name: string): string | null {
  if (!name || name.length > MAX_DISPLAY_NAME_LENGTH) {
    return `name must be 1-${MAX_DISPLAY_NAME_LENGTH} characters`
  }
  return null
}

export type AliasesValidation = { ok: true; aliases: string[] } | { ok: false; error: string }

/**
 * 1-10 entries, each a valid alias (`validateAlias`), no duplicates within
 * the payload itself — cross-group uniqueness (docs/auth.md § Model groups)
 * is a DB-backed check (`findAliasConflicts`) done by the route, not here.
 */
export function validateAliases(aliases: unknown): AliasesValidation {
  if (!Array.isArray(aliases) || aliases.length === 0) {
    return { ok: false, error: "aliases must be a non-empty array" }
  }
  if (aliases.length > MAX_ALIASES_PER_GROUP) {
    return { ok: false, error: `aliases must have at most ${MAX_ALIASES_PER_GROUP} entries` }
  }
  const out: string[] = []
  const seen = new Set<string>()
  for (const a of aliases) {
    if (typeof a !== "string") return { ok: false, error: "aliases entries must be strings" }
    const trimmed = a.trim()
    const err = validateAlias(trimmed)
    if (err) return { ok: false, error: err }
    if (seen.has(trimmed)) return { ok: false, error: `duplicate alias "${trimmed}"` }
    seen.add(trimmed)
    out.push(trimmed)
  }
  return { ok: true, aliases: out }
}

export type GroupTargetsValidation = { ok: true; targets: GroupTarget[] } | { ok: false; error: string }

/**
 * `resolvePrefix` decides whether a target's `model` prefix is a valid
 * builtin `ProviderId` or one of the caller's own custom slugs — sync, no DB.
 * `resolveAccount` decides whether a pinned `account_id` is an
 * `upstream_accounts` row owned by the caller whose `provider` matches the
 * target's prefix (docs/auth.md § Model groups) — async and DB-backed, so
 * (like `resolvePrefix`) it's injected rather than queried here; this module
 * stays free of DB access.
 *
 * Each target entry may be a bare `"provider/model"` string (shorthand for
 * `{model}`, still accepted on the wire and in storage — v3.0.0 rows) or an
 * object `{model, account_id?}`. Duplicate identity is `model` + `account_id`
 * together, so the same model pinned to two different accounts (or once
 * pinned and once not) is two legitimate targets.
 */
export async function validateGroupTargets(
  targets: unknown,
  resolvePrefix: (prefix: string) => boolean,
  resolveAccount: (accountId: string, provider: string) => Promise<boolean>,
): Promise<GroupTargetsValidation> {
  if (!Array.isArray(targets) || targets.length === 0) {
    return { ok: false, error: "targets must be a non-empty array" }
  }
  if (targets.length > MAX_TARGETS_PER_GROUP) {
    return { ok: false, error: `targets must have at most ${MAX_TARGETS_PER_GROUP} entries` }
  }
  const out: GroupTarget[] = []
  const seen = new Set<string>()
  for (const t of targets) {
    let modelRaw: unknown
    let accountIdRaw: unknown
    if (typeof t === "string") {
      modelRaw = t
      accountIdRaw = undefined
    } else if (t && typeof t === "object" && !Array.isArray(t)) {
      const obj = t as Record<string, unknown>
      modelRaw = obj.model
      accountIdRaw = obj.account_id
    } else {
      return { ok: false, error: "targets entries must be a string or a {model, account_id?} object" }
    }

    if (typeof modelRaw !== "string") {
      return { ok: false, error: "target model must be a string" }
    }
    const trimmed = modelRaw.trim()
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

    let accountId: string | null = null
    if (accountIdRaw !== undefined && accountIdRaw !== null) {
      if (typeof accountIdRaw !== "string" || !accountIdRaw) {
        return { ok: false, error: `target "${trimmed}" has an invalid account_id` }
      }
      const owned = await resolveAccount(accountIdRaw, split.prefix)
      if (!owned) {
        return {
          ok: false,
          error: `target "${trimmed}" account_id does not belong to this user's "${split.prefix}" provider`,
        }
      }
      accountId = accountIdRaw
    }

    // A NUL separator (never legal inside a target string) rules out any
    // collision between two distinct (model, account_id) pairs.
    const identity = `${trimmed}\u0000${accountId ?? ""}`
    if (seen.has(identity)) {
      return {
        ok: false,
        error: accountId
          ? `duplicate target "${trimmed}" pinned to account "${accountId}"`
          : `duplicate target "${trimmed}"`,
      }
    }
    seen.add(identity)
    out.push({ model: trimmed, account_id: accountId })
  }
  return { ok: true, targets: out }
}
