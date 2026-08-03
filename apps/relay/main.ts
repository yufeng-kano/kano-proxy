/** Entry point — wires env vars into the handler and starts the Deno HTTP server. */

import { createRelayHandler } from "./relay.ts"

Deno.serve(
  { port: Number(Deno.env.get("PORT") ?? "8080") },
  createRelayHandler({ upstreamBase: Deno.env.get("UPSTREAM_BASE") || undefined }),
)
