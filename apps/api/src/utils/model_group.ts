/** Validation and limits for user-defined model groups (docs/providers.md § Model groups). */

import type { GroupModelInput, GroupTarget } from "../db/model_groups"
import { DEFAULT_STRATEGY } from "../routing/strategy"
import { splitModelId } from "./model"

export const MAX_MODEL_GROUPS_PER_USER = 50
export const MAX_MODELS_PER_GROUP = 20
export const MAX_TARGETS_PER_MODEL = 20
export const MAX_MODEL_NAME_LENGTH = 128
export const MAX_DISPLAY_NAME_LENGTH = 64

/**
 * Same shape as a custom-provider slug (docs/providers.md § Model groups
 * "Slug") — but with **no reserved-word list**: the `/g/` path prefix is its
 * own namespace, so a group slug can never collide with a provider id or any
 * other route.
 */
const GROUP_SLUG_RE = /^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/

export function validateGroupSlug(slug: string): string | null {
  if (slug.length < 2 || slug.length > 32) {
    return "slug must be 2-32 characters"
  }
  if (!GROUP_SLUG_RE.test(slug)) {
    return "slug must be lowercase alphanumeric with hyphens, starting and ending with a letter or digit"
  }
  return null
}

/**
 * One callable model name on a group endpoint: trimmed, 1-128 chars, no
 * whitespace. Unlike the v3 bare-name aliases, `/` **is allowed** — a group
 * endpoint has no `provider/model` resolution to collide with, so a group
 * can mirror full ids as well as bare ones.
 */
export function validateModelName(name: string): string | null {
  if (!name || name.length > MAX_MODEL_NAME_LENGTH) {
    return `model name must be 1-${MAX_MODEL_NAME_LENGTH} characters`
  }
  if (/\s/.test(name)) return "model name must not contain whitespace"
  return null
}

/**
 * A group's display name: trimmed, 1-64 chars, free text (spaces fine — a
 * label, never part of the URL; the callable surface is the slug + models).
 */
export function validateDisplayName(name: string): string | null {
  if (!name || name.length > MAX_DISPLAY_NAME_LENGTH) {
    return `name must be 1-${MAX_DISPLAY_NAME_LENGTH} characters`
  }
  return null
}

/**
 * `strategy` (docs/providers.md § Routing module): `ordered` is the only
 * accepted value today, so this is a strict equality check, not a set
 * membership one — a future second value adds a branch here, not just to
 * the set literal. `undefined` (field omitted) is the caller's job to
 * default, not this validator's — see the POST/PUT routes.
 */
export function validateStrategy(strategy: unknown): string | null {
  if (strategy !== DEFAULT_STRATEGY) {
    return `strategy must be "${DEFAULT_STRATEGY}"`
  }
  return null
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
 * `{model}`, still accepted on the wire and in storage) or an object
 * `{model, account_id?}`. Duplicate identity is `model` + `account_id`
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
  if (targets.length > MAX_TARGETS_PER_MODEL) {
    return { ok: false, error: `targets must have at most ${MAX_TARGETS_PER_MODEL} entries` }
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
    // spell it out: a group can never target another group's model — no
    // nesting, no cycles, structurally.
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

export type GroupModelsValidation = { ok: true; models: GroupModelInput[] } | { ok: false; error: string }

/**
 * The group's whole model set: 1-20 entries of `{name, targets}`, names
 * unique **within the payload** (= within the group — the set replaces the
 * stored one wholesale, and other groups may reuse a name freely since the
 * endpoint is the namespace). Each entry's targets go through
 * `validateGroupTargets`, with the model's name prefixed onto any error so
 * the message says which model it is about.
 */
export async function validateGroupModels(
  models: unknown,
  resolvePrefix: (prefix: string) => boolean,
  resolveAccount: (accountId: string, provider: string) => Promise<boolean>,
): Promise<GroupModelsValidation> {
  if (!Array.isArray(models) || models.length === 0) {
    return { ok: false, error: "models must be a non-empty array" }
  }
  if (models.length > MAX_MODELS_PER_GROUP) {
    return { ok: false, error: `models must have at most ${MAX_MODELS_PER_GROUP} entries` }
  }
  const out: GroupModelInput[] = []
  const seen = new Set<string>()
  for (const entry of models) {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
      return { ok: false, error: "models entries must be {name, targets} objects" }
    }
    const obj = entry as Record<string, unknown>
    if (typeof obj.name !== "string") {
      return { ok: false, error: "model name must be a string" }
    }
    const name = obj.name.trim()
    const nameErr = validateModelName(name)
    if (nameErr) return { ok: false, error: nameErr }
    if (seen.has(name)) return { ok: false, error: `duplicate model name "${name}"` }
    seen.add(name)

    const targetsRes = await validateGroupTargets(obj.targets, resolvePrefix, resolveAccount)
    if (!targetsRes.ok) {
      return { ok: false, error: `model "${name}": ${targetsRes.error}` }
    }
    out.push({ name, targets: targetsRes.targets })
  }
  return { ok: true, models: out }
}
