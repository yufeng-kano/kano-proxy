/**
 * The AgentTunnel's request multiplexer (docs/cli.md § Wire protocol /
 * § Failover semantics), kept free of Workers-runtime APIs so tests can
 * drive it with in-memory frames: the DO class in agent_tunnel.ts wires a
 * real hibernatable WebSocket to this, tests wire a recording stub.
 *
 * One mux serves one live socket. In-flight state is memory-only by design —
 * if the DO is evicted mid-request the socket drops and the CLI aborts its
 * local requests; frames that arrive for an id nobody awaits are answered
 * with `cancel` so the CLI stops streaming into the void.
 */

import {
  BODY_KIND_REQUEST,
  BODY_KIND_RESPONSE,
  FIRST_RES_TIMEOUT_MS,
  MAX_CHUNK_BYTES,
  MAX_INFLIGHT,
  REQUEST_BODY_LIMIT_BYTES,
  RESPONSE_BUFFER_LIMIT_BYTES,
  decodeBinaryFrame,
  encodeBinaryFrame,
  faultFromResErrReason,
  parseControlFrame,
  validateModelsReport,
  type AgentFaultReason,
} from "./protocol"

export const AGENT_UPSTREAM_HEADER = "x-agent-upstream"
export const AGENT_FAULT_HEADER = "x-agent-fault"

export function agentFaultResponse(reason: AgentFaultReason): Response {
  return new Response(JSON.stringify({ error: { type: "agent_fault", reason } }), {
    status: 502,
    headers: { [AGENT_FAULT_HEADER]: reason, "content-type": "application/json" },
  })
}

export type MuxSocket = {
  send(data: string | Uint8Array): void
}

export type MuxHooks = {
  /** A valid `models` frame arrived — persist the report (docs/cli.md § Model catalog). */
  onModelsReport?(models: string[]): void | Promise<void>
}

type PendingRequest = {
  settle: (res: Response) => void
  settled: boolean
  controller: ReadableStreamDefaultController<Uint8Array> | null
  firstResTimer: ReturnType<typeof setTimeout> | null
}

export class TunnelMux {
  private pending = new Map<number, PendingRequest>()
  private nextId = 1

  constructor(
    private socket: MuxSocket,
    private hooks: MuxHooks = {},
    private firstResTimeoutMs: number = FIRST_RES_TIMEOUT_MS,
  ) {}

  inflightCount(): number {
    return this.pending.size
  }

  /**
   * Open one proxied request over the socket. Resolves with either the local
   * server's answer (marked `x-agent-upstream: 1`, status passed through) or
   * a tunnel fault (502 + `x-agent-fault`). The caller has already enforced
   * the path allowlist and reduced the headers.
   */
  openRequest(opts: {
    method: string
    path: string
    headers: Record<string, string>
    body: ReadableStream<Uint8Array> | null
  }): Promise<Response> {
    if (this.pending.size >= MAX_INFLIGHT) {
      return Promise.resolve(agentFaultResponse("busy"))
    }
    const id = this.nextId++
    return new Promise<Response>((resolve) => {
      const entry: PendingRequest = {
        settled: false,
        settle: (res) => {
          if (entry.settled) return
          entry.settled = true
          resolve(res)
        },
        controller: null,
        firstResTimer: null,
      }
      this.pending.set(id, entry)
      try {
        this.socket.send(
          JSON.stringify({ t: "req", id, method: opts.method, path: opts.path, headers: opts.headers }),
        )
      } catch {
        this.pending.delete(id)
        entry.settle(agentFaultResponse("offline"))
        return
      }
      void this.pumpRequestBody(id, entry, opts.body)
    })
  }

  private async pumpRequestBody(
    id: number,
    entry: PendingRequest,
    body: ReadableStream<Uint8Array> | null,
  ): Promise<void> {
    try {
      if (body) {
        const reader = body.getReader()
        let sentBytes = 0
        try {
          for (;;) {
            const { done, value } = await reader.read()
            if (done) break
            if (!this.pending.has(id)) {
              await reader.cancel()
              return
            }
            if (value) {
              // ws.send has no drain signal, so a body arriving faster than
              // the socket empties would queue unboundedly in DO memory — the
              // hard cap is the honest request-side bound (docs/cli.md).
              sentBytes += value.byteLength
              if (sentBytes > REQUEST_BODY_LIMIT_BYTES) {
                await reader.cancel()
                this.failPending(id, "too_large", true)
                return
              }
              for (let offset = 0; offset < value.byteLength; offset += MAX_CHUNK_BYTES) {
                this.socket.send(
                  encodeBinaryFrame(id, BODY_KIND_REQUEST, value.subarray(offset, offset + MAX_CHUNK_BYTES)),
                )
              }
            }
          }
        } finally {
          reader.releaseLock()
        }
      }
      if (!this.pending.has(id)) return
      this.socket.send(JSON.stringify({ t: "req_end", id }))
      entry.firstResTimer = setTimeout(() => this.failPending(id, "timeout", true), this.firstResTimeoutMs)
    } catch {
      // Reading the client's request body failed (usually a client abort) —
      // tell the CLI to drop the request rather than leaving it half-sent.
      this.failPending(id, "protocol", true)
    }
  }

  /** Socket gone (close/error/replaced/expired): every open request faults. */
  abortAll(reason: AgentFaultReason): void {
    for (const id of [...this.pending.keys()]) {
      this.failPending(id, reason, false)
    }
  }

  private failPending(id: number, reason: AgentFaultReason, sendCancel: boolean): void {
    const entry = this.pending.get(id)
    if (!entry) return
    this.cleanup(id, entry)
    if (sendCancel) this.trySendCancel(id)
    if (!entry.settled) {
      entry.settle(agentFaultResponse(reason))
    } else if (entry.controller) {
      try {
        entry.controller.error(new Error(`agent tunnel: ${reason}`))
      } catch {
        /* already errored/closed */
      }
    }
  }

  private cleanup(id: number, entry: PendingRequest): void {
    if (entry.firstResTimer !== null) clearTimeout(entry.firstResTimer)
    entry.firstResTimer = null
    this.pending.delete(id)
  }

  private trySendCancel(id: number): void {
    try {
      this.socket.send(JSON.stringify({ t: "cancel", id }))
    } catch {
      /* socket already dead — nothing to cancel against */
    }
  }

  /** End-client disconnected while the response streamed — propagate the abort down the tunnel. */
  private onResponseCancelled(id: number): void {
    const entry = this.pending.get(id)
    if (!entry) return
    this.cleanup(id, entry)
    this.trySendCancel(id)
  }

  /**
   * Returns a promise exactly when the frame started async work (a models
   * report's D1 write): the DO awaits it so a hibernatable event cannot end
   * before the persistence lands. Everything else is synchronous.
   */
  handleMessage(data: string | ArrayBuffer): Promise<void> | void {
    if (typeof data !== "string") {
      const frame = decodeBinaryFrame(data)
      if (!frame || frame.kind !== BODY_KIND_RESPONSE) return
      this.handleResponseChunk(frame.id, frame.chunk)
      return
    }
    const frame = parseControlFrame(data)
    if (!frame) return
    switch (frame.t) {
      case "res":
        this.handleRes(frame.id, frame.status, frame.headers)
        return
      case "res_end": {
        const entry = this.pending.get(frame.id)
        if (!entry) return
        this.cleanup(frame.id, entry)
        try {
          entry.controller?.close()
        } catch {
          /* already closed */
        }
        return
      }
      case "res_err":
        this.failPending(frame.id, faultFromResErrReason(frame.reason), false)
        return
      case "cancel":
        // The CLI's local abort raced a partial response — drop it like res_err.
        this.failPending(frame.id, "protocol", false)
        return
      case "models": {
        const models = validateModelsReport(frame.models)
        // An out-of-bounds report is ignored whole — the last good report stays.
        if (models && this.hooks.onModelsReport) {
          return Promise.resolve(this.hooks.onModelsReport(models))
        }
        return
      }
      default:
        return
    }
  }

  private handleRes(id: number, status: number, headers: Record<string, string>): void {
    const entry = this.pending.get(id)
    if (!entry) {
      // A response for a request nobody awaits (in-flight state lost to an
      // eviction) — tell the CLI to stop streaming it.
      this.trySendCancel(id)
      return
    }
    if (entry.settled) return
    if (entry.firstResTimer !== null) {
      clearTimeout(entry.firstResTimer)
      entry.firstResTimer = null
    }

    const responseHeaders = new Headers({ [AGENT_UPSTREAM_HEADER]: "1" })
    // Header reduction discipline (docs/cli.md): content-type only.
    const contentType = headers["content-type"]
    if (contentType) responseHeaders.set("content-type", contentType)

    // Statuses that must not carry a body get none; the CLI still sends
    // res_end, which cleanup below tolerates as an unknown id.
    if (status === 204 || status === 205 || status === 304) {
      this.cleanup(id, entry)
      entry.settle(new Response(null, { status, headers: responseHeaders }))
      return
    }

    const mux = this
    const body = new ReadableStream<Uint8Array>(
      {
        start(controller) {
          entry.controller = controller
        },
        cancel() {
          mux.onResponseCancelled(id)
        },
      },
      new ByteLengthQueuingStrategy({ highWaterMark: RESPONSE_BUFFER_LIMIT_BYTES }),
    )
    entry.settle(new Response(body, { status, headers: responseHeaders }))
  }

  private handleResponseChunk(id: number, chunk: Uint8Array): void {
    const entry = this.pending.get(id)
    if (!entry) {
      this.trySendCancel(id)
      return
    }
    if (!entry.controller) return
    try {
      entry.controller.enqueue(chunk)
    } catch {
      this.cleanup(id, entry)
      return
    }
    // The end client reads slower than the CLI sends and the gap passed the
    // cap: cancel rather than buffer without bound (docs/cli.md — `too_large`).
    const desired = entry.controller.desiredSize
    if (desired !== null && desired < 0) {
      this.failPending(id, "too_large", true)
    }
  }
}
