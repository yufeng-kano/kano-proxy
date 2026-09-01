/**
 * Shared between the token-authenticated /agent/v1 surface (routes/agent.ts)
 * and the session-authenticated /api/cli surface (routes/cli.ts): the
 * provider list item (with the AgentTunnel DO connection read-through) and
 * the delete path both exist on both surfaces (docs/cli.md).
 */

import {
  deleteCliProvider,
  exposedCliModels,
  listCliDevices,
  listCliProviders,
  parseCliModels,
  type CliProviderRow,
} from "../db/cli"
import { deleteAccountsForProvider, listAccounts } from "../db/accounts"
import type { Env } from "../env"
import { clearBench } from "../pool/bench"

export function agentTunnelStub(env: Env, providerId: string): DurableObjectStub | null {
  const namespace = env.AGENT_TUNNEL
  if (!namespace) return null
  return namespace.get(namespace.idFromName(providerId))
}

async function tunnelConnected(env: Env, providerId: string): Promise<boolean> {
  const stub = agentTunnelStub(env, providerId)
  if (!stub) return false
  try {
    const res = await stub.fetch("https://agent-tunnel/status")
    if (!res.ok) return false
    const json = (await res.json()) as { connected?: boolean }
    return json.connected === true
  } catch {
    return false
  }
}

export async function cliProviderListItem(
  env: Env,
  row: CliProviderRow,
  deviceNames: Map<string, string>,
): Promise<Record<string, unknown>> {
  // The internal pool-state row's id — the handle the Groups picker pins a
  // target to, exactly like a custom endpoint's key row (docs/admin-ui.md
  // § Groups page). Not a user-facing account; it carries no credential.
  const accounts = await listAccounts(env.DB, row.user_id, row.slug)
  return {
    id: row.id,
    slug: row.slug,
    name: row.name,
    format: row.format,
    connected: await tunnelConnected(env, row.id),
    account_id: accounts[0]?.id ?? null,
    models: exposedCliModels(row),
    models_reported: parseCliModels(row.models_json).length,
    model_filter: parseCliModels(row.model_filter_json),
    models_updated_at: row.models_updated_at,
    device_id: row.device_id,
    device_name: row.device_id ? (deviceNames.get(row.device_id) ?? null) : null,
    created_at: row.created_at,
    updated_at: row.updated_at,
  }
}

export async function listCliProviderItems(env: Env, userId: string): Promise<Record<string, unknown>[]> {
  const rows = await listCliProviders(env.DB, userId)
  if (rows.length === 0) return []
  // One query for every device name rather than one per provider — a
  // provider's device_id always belongs to the same user, so the user's
  // device list covers them all (revoked included).
  const deviceNames = new Map(
    (await listCliDevices(env.DB, userId)).map((device) => [device.id, device.name]),
  )
  return Promise.all(rows.map((row) => cliProviderListItem(env, row, deviceNames)))
}

/** Provider + internal account rows + bench keys + live socket, in that order. */
export async function removeCliProvider(env: Env, userId: string, row: CliProviderRow): Promise<void> {
  const removedAccounts = await deleteAccountsForProvider(env.DB, userId, row.slug)
  await deleteCliProvider(env.DB, userId, row.id)
  for (const acc of removedAccounts) {
    try {
      await clearBench(env, userId, row.slug, acc.id)
    } catch {
      /* best-effort */
    }
  }
  const stub = agentTunnelStub(env, row.id)
  if (stub) {
    try {
      await stub.fetch("https://agent-tunnel/close", { method: "POST" })
    } catch {
      /* best-effort — an unreachable DO has no socket to close */
    }
  }
}
