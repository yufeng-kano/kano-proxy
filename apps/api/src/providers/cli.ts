/**
 * CLI-provider adapters (docs/cli.md § Failover semantics "Routing
 * integration"): the custom adapters unchanged, with an injected fetch that
 * goes to the provider's AgentTunnel DO stub instead of the network. Format
 * semantics (`openai` near-passthrough / `anthropic` native passthrough,
 * conversion paths, loop guard, count_tokens sentinel) are inherited; only
 * the transport differs.
 */

import type { CliProviderRow } from "../db/cli"
import type { CustomProviderRow } from "../db/custom_providers"
import { INTERNAL_FORMAT_HEADER } from "../do/agent_tunnel"
import { agentFaultResponse } from "../do/tunnel_mux"
import type { Env } from "../env"
import { createCustomAnthropicAdapter } from "./custom_anthropic"
import { createCustomOpenAIAdapter } from "./custom_openai"
import type { ProviderAdapter } from "./types"

/**
 * The injected transport. The custom adapters build URLs as
 * `${base_url}<suffix>` and the pseudo-row's base is empty, so `input` here
 * is exactly the bare allowlisted suffix (`/chat/completions`, `/v1/messages`,
 * …) that goes on the wire — the CLI joins it onto its one configured target
 * base. Auth headers are stripped: the placeholder credential means nothing,
 * and the CLI injects the local server's own key on its side.
 */
function createAgentFetch(env: Env, row: CliProviderRow): typeof fetch {
  return async (input, init) => {
    const namespace = env.AGENT_TUNNEL
    if (!namespace) return agentFaultResponse("offline")
    const path = typeof input === "string" ? input : input instanceof URL ? input.pathname : new URL(input.url).pathname
    const headers = new Headers(init?.headers)
    headers.delete("authorization")
    headers.delete("x-api-key")
    headers.set(INTERNAL_FORMAT_HEADER, row.format)
    const stub = namespace.get(namespace.idFromName(row.id))
    try {
      return await stub.fetch(`https://agent-tunnel${path}`, { ...init, headers })
    } catch {
      // Never reached the DO app (platform error): 502, no bench, and no
      // fault marker — the "neither marker" row of the tri-state table.
      return new Response(JSON.stringify({ error: { type: "agent_fault", reason: "unreachable" } }), {
        status: 502,
        headers: { "content-type": "application/json" },
      })
    }
  }
}

/** The custom adapters read only these fields; base_url "" makes every path a bare suffix. */
function pseudoCustomRow(row: CliProviderRow): CustomProviderRow {
  return {
    id: row.id,
    user_id: row.user_id,
    slug: row.slug,
    name: row.name,
    format: row.format,
    base_url: "",
    count_tokens_url: null,
    models_mode: "manual",
    manual_models_json: row.models_json,
    sort_order: row.sort_order,
    created_at: row.created_at,
    updated_at: row.updated_at,
  }
}

export function createCliAdapter(env: Env, row: CliProviderRow): ProviderAdapter {
  const agentFetch = createAgentFetch(env, row)
  const adapter =
    row.format === "anthropic"
      ? createCustomAnthropicAdapter(pseudoCustomRow(row), agentFetch)
      : createCustomOpenAIAdapter(pseudoCustomRow(row), agentFetch)
  // The catalog is agent-reported, never pulled (docs/cli.md § Model catalog)
  // — drop the inherited live listModels so nothing ever queries the tunnel
  // for a list the D1 report already answers.
  delete adapter.listModels
  return adapter
}
