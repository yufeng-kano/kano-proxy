import type { Context } from "hono"
import type { HonoEnv } from "../auth/session"
import { getGroupBySlug } from "../db/model_groups"
import {
  resolveCandidates,
  resolveGroupModelCandidates,
  type RoutingResolution,
} from "../routing/candidates"

export type RequestModelResolution =
  | { kind: "ok"; resolution: RoutingResolution }
  | { kind: "group_not_found"; slug: string }
  | { kind: "invalid_model"; groupSlug?: string }

/**
 * Model resolution for both surface mounts (docs/api.md "Model routing"):
 * the shared bases resolve `provider/model` directly; a `/g/:slug/…` mount
 * resolves the slug to the caller's group first (unknown slug → the route's
 * 404 — docs/api.md § Group endpoints) and then the request's `model` within
 * that group. The `slug` param exists only on the group mounts, which is
 * what selects the branch — the handlers themselves are shared.
 */
export async function resolveRequestModel(
  c: Context<HonoEnv>,
  userId: string,
  modelRaw: string,
): Promise<RequestModelResolution> {
  const slug = c.req.param("slug")
  if (slug === undefined) {
    const resolution = await resolveCandidates(c.env, userId, modelRaw)
    return resolution ? { kind: "ok", resolution } : { kind: "invalid_model" }
  }
  const group = await getGroupBySlug(c.env.DB, userId, slug)
  if (!group) return { kind: "group_not_found", slug }
  const resolution = await resolveGroupModelCandidates(c.env, userId, group, modelRaw)
  return resolution ? { kind: "ok", resolution } : { kind: "invalid_model", groupSlug: slug }
}
