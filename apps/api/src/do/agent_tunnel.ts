/**
 * AgentTunnel Durable Object (docs/cli.md § AgentTunnel Durable Object).
 * One DO per CLI provider (`idFromName(cli_providers.id)`), holding at most
 * one live hibernatable WebSocket to the `kano-proxy start` process. The
 * Worker routes provider traffic here through the internal paths below; the
 * multiplexing itself lives in tunnel_mux.ts.
 *
 * Auth happens in the Worker before the stub is reached: the DO trusts the
 * `x-kano-*` internal headers and never sees tokens (docs/cli.md § Wire
 * protocol). Wake sources: a proxied request, a real frame from the CLI, the
 * token-expiry alarm, and admin status fetches — heartbeats are handled by
 * the auto-response pair without waking anything.
 */

import type { Env } from "../env"
import {
  AGENT_PROTO,
  CLOSE_REPLACED,
  CLOSE_TOKEN_EXPIRED,
  isAllowedPath,
  type CliProviderFormat,
} from "./protocol"
import { TunnelMux, agentFaultResponse } from "./tunnel_mux"

/** Trusted internal headers the Worker sets when forwarding into the stub. */
export const INTERNAL_USER_HEADER = "x-kano-user-id"
export const INTERNAL_PROVIDER_HEADER = "x-kano-provider-id"
export const INTERNAL_SLUG_HEADER = "x-kano-slug"
export const INTERNAL_FORMAT_HEADER = "x-kano-format"
export const INTERNAL_EXP_HEADER = "x-kano-token-exp"

type SocketAttachment = {
  userId: string
  providerId: string
  slug: string
  format: CliProviderFormat
}

/** Request headers forwarded down the tunnel — same reduction discipline as the codex relay. */
const FORWARDED_REQUEST_HEADERS = ["content-type", "accept", "anthropic-version", "anthropic-beta"]

export class AgentTunnel implements DurableObject {
  private mux: TunnelMux | null = null
  private muxSocket: WebSocket | null = null

  constructor(
    private state: DurableObjectState,
    private env: Env,
  ) {}

  private activeSocket(): WebSocket | null {
    const sockets = this.state.getWebSockets()
    return sockets.length > 0 ? sockets[0]! : null
  }

  private attachmentOf(ws: WebSocket): SocketAttachment | null {
    try {
      const meta = ws.deserializeAttachment() as SocketAttachment | null
      return meta && typeof meta === "object" ? meta : null
    } catch {
      return null
    }
  }

  /** In-flight state is memory-only; a woken DO rebuilds an empty mux bound to the surviving socket. */
  private muxFor(ws: WebSocket): TunnelMux {
    if (this.mux && this.muxSocket === ws) return this.mux
    const meta = this.attachmentOf(ws)
    this.muxSocket = ws
    this.mux = new TunnelMux(
      { send: (data) => ws.send(data) },
      {
        onModelsReport: async (models) => {
          if (!meta) return
          try {
            const { writeCliProviderModels } = await import("../db/cli")
            await writeCliProviderModels(this.env.DB, meta.providerId, models)
          } catch (error) {
            console.error("[agent-tunnel] models report write failed", {
              providerId: meta?.providerId,
              error: error instanceof Error ? error.message : String(error),
            })
          }
        },
      },
    )
    return this.mux
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url)

    if (url.pathname === "/connect") {
      return this.handleConnect(request)
    }
    if (url.pathname === "/status") {
      const ws = this.activeSocket()
      return Response.json({ connected: ws !== null, proto: AGENT_PROTO })
    }
    if (url.pathname === "/close") {
      this.closeAll(1000, "closed")
      return Response.json({ ok: true })
    }
    return this.handleProxiedRequest(request, url.pathname)
  }

  private async handleConnect(request: Request): Promise<Response> {
    if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
      return Response.json({ error: "expected websocket" }, { status: 426 })
    }
    const userId = request.headers.get(INTERNAL_USER_HEADER)
    const providerId = request.headers.get(INTERNAL_PROVIDER_HEADER)
    const slug = request.headers.get(INTERNAL_SLUG_HEADER)
    const format = request.headers.get(INTERNAL_FORMAT_HEADER)
    const exp = Number(request.headers.get(INTERNAL_EXP_HEADER))
    if (!userId || !providerId || !slug || (format !== "openai" && format !== "anthropic") || !Number.isFinite(exp)) {
      return Response.json({ error: "missing internal connect headers" }, { status: 400 })
    }

    // One live socket: a second successful connect replaces the first —
    // laptop-resume reconnects are self-healing instead of "address in use".
    this.mux?.abortAll("replaced")
    this.mux = null
    this.muxSocket = null
    for (const old of this.state.getWebSockets()) {
      try {
        old.close(CLOSE_REPLACED, "replaced")
      } catch {
        /* already closing */
      }
    }

    const pair = new WebSocketPair()
    const [client, server] = [pair[0], pair[1]]
    this.state.acceptWebSocket(server)
    server.serializeAttachment({ userId, providerId, slug, format } satisfies SocketAttachment)
    // Idle heartbeats never wake the DO and never bill duration.
    this.state.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"))
    // The only scheduled work: revocation reaches a live socket at access-token
    // expiry (an alarm wakes a hibernated DO; a setTimeout would keep it awake).
    await this.state.storage.setAlarm(exp)
    server.send(JSON.stringify({ t: "hello", proto: AGENT_PROTO, slug }))

    // Reconnect clears the bench: opening the laptop restores service on the
    // next request, no operator action (docs/cli.md § Failover semantics).
    try {
      await this.env.DB.prepare(
        `UPDATE upstream_accounts SET bench_until = NULL, bench_reason = NULL WHERE user_id = ? AND provider = ?`,
      )
        .bind(userId, slug)
        .run()
    } catch (error) {
      console.error("[agent-tunnel] bench clear on connect failed", {
        providerId,
        error: error instanceof Error ? error.message : String(error),
      })
    }

    return new Response(null, { status: 101, webSocket: client })
  }

  private async handleProxiedRequest(request: Request, path: string): Promise<Response> {
    const format = request.headers.get(INTERNAL_FORMAT_HEADER)
    if (format !== "openai" && format !== "anthropic") {
      return agentFaultResponse("protocol")
    }
    if (!isAllowedPath(format, path)) {
      return agentFaultResponse("protocol")
    }
    const ws = this.activeSocket()
    if (!ws) return agentFaultResponse("offline")

    const headers: Record<string, string> = {}
    for (const name of FORWARDED_REQUEST_HEADERS) {
      const value = request.headers.get(name)
      if (value) headers[name] = value
    }

    return this.muxFor(ws).openRequest({
      method: request.method,
      path,
      headers,
      body: request.body,
    })
  }

  private closeAll(code: number, reason: string): void {
    this.mux?.abortAll(code === CLOSE_TOKEN_EXPIRED ? "offline" : "replaced")
    this.mux = null
    this.muxSocket = null
    for (const ws of this.state.getWebSockets()) {
      try {
        ws.close(code, reason)
      } catch {
        /* already closing */
      }
    }
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    if (this.muxSocket && this.muxSocket !== ws) return
    this.muxFor(ws).handleMessage(message)
  }

  async webSocketClose(ws: WebSocket): Promise<void> {
    if (this.muxSocket === ws || this.muxSocket === null) {
      this.mux?.abortAll("offline")
      this.mux = null
      this.muxSocket = null
    }
  }

  async webSocketError(ws: WebSocket): Promise<void> {
    return this.webSocketClose(ws)
  }

  /** Access-token expiry: the CLI treats 4003 as "refresh, then reconnect". */
  async alarm(): Promise<void> {
    this.closeAll(CLOSE_TOKEN_EXPIRED, "token_expired")
  }
}
