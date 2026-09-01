/**
 * Protocol-level tests for the AgentTunnel multiplexer (docs/cli.md § Wire
 * protocol) — an in-memory socket records what the DO side sends, and frames
 * are fed back as the CLI would send them. No Workers runtime involved.
 */
import { describe, expect, it, vi } from "vitest"
import {
  BODY_KIND_REQUEST,
  BODY_KIND_RESPONSE,
  decodeBinaryFrame,
  encodeBinaryFrame,
  parseControlFrame,
  validateModelsReport,
} from "../src/do/protocol"
import { TunnelMux, agentFaultResponse } from "../src/do/tunnel_mux"

type SentFrame = { control?: Record<string, unknown>; binary?: { id: number; kind: number; chunk: Uint8Array } }

function recordingSocket() {
  const sent: SentFrame[] = []
  return {
    sent,
    controls(type?: string): Record<string, unknown>[] {
      const all = sent.filter((f) => f.control).map((f) => f.control!)
      return type ? all.filter((f) => f.t === type) : all
    },
    binaries(): { id: number; kind: number; chunk: Uint8Array }[] {
      return sent.filter((f) => f.binary).map((f) => f.binary!)
    },
    socket: {
      send(data: string | Uint8Array) {
        if (typeof data === "string") {
          sent.push({ control: JSON.parse(data) as Record<string, unknown> })
        } else {
          const buf = data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) as ArrayBuffer
          sent.push({ binary: decodeBinaryFrame(buf)! })
        }
      },
    },
  }
}

function frame(obj: Record<string, unknown>): string {
  return JSON.stringify(obj)
}

function binary(id: number, kind: number, chunk: Uint8Array): ArrayBuffer {
  const encoded = encodeBinaryFrame(id, kind, chunk)
  return encoded.buffer.slice(encoded.byteOffset, encoded.byteOffset + encoded.byteLength) as ArrayBuffer
}

const text = (s: string) => new TextEncoder().encode(s)

async function flush(): Promise<void> {
  await new Promise((r) => setTimeout(r, 0))
}

describe("binary framing", () => {
  it("round-trips id, kind and chunk", () => {
    const encoded = encodeBinaryFrame(70000, BODY_KIND_RESPONSE, text("hello"))
    const decoded = decodeBinaryFrame(encoded.buffer.slice(0) as ArrayBuffer)!
    expect(decoded.id).toBe(70000)
    expect(decoded.kind).toBe(BODY_KIND_RESPONSE)
    expect(new TextDecoder().decode(decoded.chunk)).toBe("hello")
  })

  it("rejects a truncated frame", () => {
    expect(decodeBinaryFrame(new ArrayBuffer(3))).toBeNull()
  })
})

describe("control frame parsing", () => {
  it("parses known frames and rejects junk", () => {
    expect(parseControlFrame(frame({ t: "res", id: 1, status: 200, headers: { "content-type": "a" } }))).toMatchObject(
      { t: "res", id: 1, status: 200 },
    )
    expect(parseControlFrame(frame({ t: "res_end", id: 2 }))).toEqual({ t: "res_end", id: 2 })
    expect(parseControlFrame("not json")).toBeNull()
    expect(parseControlFrame(frame({ t: "unknown" }))).toBeNull()
    expect(parseControlFrame(frame({ t: "res", id: "x", status: 200 }))).toBeNull()
  })
})

describe("models report validation", () => {
  it("bounds entries like the custom manual list", () => {
    expect(validateModelsReport(["llama3.3:70b", "org/model"])).toEqual(["llama3.3:70b", "org/model"])
    expect(validateModelsReport(["has space"])).toBeNull()
    expect(validateModelsReport([""])).toBeNull()
    expect(validateModelsReport(["x".repeat(129)])).toBeNull()
    expect(validateModelsReport(Array.from({ length: 101 }, (_, i) => `m${i}`))).toBeNull()
  })
})

describe("TunnelMux request lifecycle", () => {
  it("sends req + body chunks + req_end, then streams the response through", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    const body = new Blob([text("request-body")]).stream()
    const promise = mux.openRequest({
      method: "POST",
      path: "/chat/completions",
      headers: { "content-type": "application/json" },
      body,
    })
    await flush()

    expect(rec.controls("req")).toMatchObject([
      { t: "req", id: 1, method: "POST", path: "/chat/completions" },
    ])
    expect(rec.binaries().map((b) => b.kind)).toEqual([BODY_KIND_REQUEST])
    expect(rec.controls("req_end")).toHaveLength(1)

    mux.handleMessage(frame({ t: "res", id: 1, status: 200, headers: { "content-type": "text/event-stream" } }))
    const res = await promise
    expect(res.status).toBe(200)
    expect(res.headers.get("x-agent-upstream")).toBe("1")
    expect(res.headers.get("content-type")).toBe("text/event-stream")

    mux.handleMessage(binary(1, BODY_KIND_RESPONSE, text("data: hi\n\n")))
    mux.handleMessage(frame({ t: "res_end", id: 1 }))
    expect(await res.text()).toBe("data: hi\n\n")
    expect(mux.inflightCount()).toBe(0)
  })

  it("passes a local error status through as a real upstream answer", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    const promise = mux.openRequest({ method: "POST", path: "/chat/completions", headers: {}, body: null })
    await flush()
    mux.handleMessage(frame({ t: "res", id: 1, status: 429, headers: { "content-type": "application/json" } }))
    mux.handleMessage(binary(1, BODY_KIND_RESPONSE, text("{}")))
    mux.handleMessage(frame({ t: "res_end", id: 1 }))
    const res = await promise
    expect(res.status).toBe(429)
    expect(res.headers.get("x-agent-upstream")).toBe("1")
    expect(res.headers.get("x-agent-fault")).toBeNull()
  })

  it("refuses the fifth concurrent request with fault busy", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    for (let i = 0; i < 4; i++) {
      void mux.openRequest({ method: "POST", path: "/chat/completions", headers: {}, body: null })
    }
    await flush()
    const res = await mux.openRequest({ method: "POST", path: "/chat/completions", headers: {}, body: null })
    expect(res.status).toBe(502)
    expect(res.headers.get("x-agent-fault")).toBe("busy")
  })

  it("maps res_err connect_refused to fault offline", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    const promise = mux.openRequest({ method: "POST", path: "/v1/messages", headers: {}, body: null })
    await flush()
    mux.handleMessage(frame({ t: "res_err", id: 1, reason: "connect_refused" }))
    const res = await promise
    expect(res.status).toBe(502)
    expect(res.headers.get("x-agent-fault")).toBe("offline")
  })

  it("faults timeout when no res arrives after req_end", async () => {
    vi.useFakeTimers()
    try {
      const rec = recordingSocket()
      const mux = new TunnelMux(rec.socket, {}, 1000)
      const promise = mux.openRequest({ method: "POST", path: "/chat/completions", headers: {}, body: null })
      await Promise.resolve()
      await Promise.resolve()
      await vi.advanceTimersByTimeAsync(1001)
      const res = await promise
      expect(res.headers.get("x-agent-fault")).toBe("timeout")
      expect(rec.controls("cancel")).toMatchObject([{ t: "cancel", id: 1 }])
    } finally {
      vi.useRealTimers()
    }
  })

  it("abortAll faults every open request", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    const a = mux.openRequest({ method: "POST", path: "/chat/completions", headers: {}, body: null })
    const b = mux.openRequest({ method: "POST", path: "/chat/completions", headers: {}, body: null })
    await flush()
    mux.abortAll("offline")
    expect((await a).headers.get("x-agent-fault")).toBe("offline")
    expect((await b).headers.get("x-agent-fault")).toBe("offline")
  })

  it("errors an in-flight stream when the socket dies mid-response", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    const promise = mux.openRequest({ method: "POST", path: "/chat/completions", headers: {}, body: null })
    await flush()
    mux.handleMessage(frame({ t: "res", id: 1, status: 200, headers: {} }))
    const res = await promise
    mux.handleMessage(binary(1, BODY_KIND_RESPONSE, text("partial")))
    mux.abortAll("offline")
    await expect(res.text()).rejects.toThrow()
  })

  it("refuses an inbound binary frame past the 1 MiB bound", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    const promise = mux.openRequest({ method: "POST", path: "/chat/completions", headers: {}, body: null })
    await flush()
    mux.handleMessage(binary(1, BODY_KIND_RESPONSE, new Uint8Array(1024 * 1024 + 1)))
    const res = await promise
    expect(res.headers.get("x-agent-fault")).toBe("protocol")
    expect(mux.inflightCount()).toBe(0)
  })

  it("cancels a response nobody awaits (post-eviction id)", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    mux.handleMessage(frame({ t: "res", id: 42, status: 200, headers: {} }))
    expect(rec.controls("cancel")).toMatchObject([{ t: "cancel", id: 42 }])
  })

  it("cancels down the tunnel when the client stops reading past the buffer cap", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    const promise = mux.openRequest({ method: "POST", path: "/chat/completions", headers: {}, body: null })
    await flush()
    mux.handleMessage(frame({ t: "res", id: 1, status: 200, headers: {} }))
    const res = await promise
    // 9 x 1 MiB unread chunks exceed the 8 MiB bound.
    const chunk = new Uint8Array(1024 * 1024)
    for (let i = 0; i < 9; i++) {
      mux.handleMessage(binary(1, BODY_KIND_RESPONSE, chunk))
    }
    expect(rec.controls("cancel")).toMatchObject([{ t: "cancel", id: 1 }])
    await expect(res.text()).rejects.toThrow()
    expect(mux.inflightCount()).toBe(0)
  })

  it("propagates end-client cancellation as a cancel frame", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    const promise = mux.openRequest({ method: "POST", path: "/chat/completions", headers: {}, body: null })
    await flush()
    mux.handleMessage(frame({ t: "res", id: 1, status: 200, headers: {} }))
    const res = await promise
    await res.body!.cancel()
    expect(rec.controls("cancel")).toMatchObject([{ t: "cancel", id: 1 }])
    expect(mux.inflightCount()).toBe(0)
  })

  it("delivers valid models reports and ignores out-of-bounds ones", async () => {
    const rec = recordingSocket()
    const reports: string[][] = []
    const mux = new TunnelMux(rec.socket, { onModelsReport: (m) => void reports.push(m) })
    mux.handleMessage(frame({ t: "models", models: ["llama3.3:70b"] }))
    mux.handleMessage(frame({ t: "models", models: ["bad id"] }))
    await flush()
    expect(reports).toEqual([["llama3.3:70b"]])
  })

  it("faults too_large when the request body passes the 32 MiB cap", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    const chunk = new Uint8Array(8 * 1024 * 1024)
    const body = new Blob([chunk, chunk, chunk, chunk, chunk]).stream() // 40 MiB
    const res = await mux.openRequest({ method: "POST", path: "/audio/transcriptions", headers: {}, body })
    expect(res.headers.get("x-agent-fault")).toBe("too_large")
    expect(rec.controls("cancel")).toMatchObject([{ t: "cancel", id: 1 }])
    expect(mux.inflightCount()).toBe(0)
  })

  it("handleMessage returns the models hook's promise so the DO can await it", async () => {
    const rec = recordingSocket()
    let persisted = false
    const mux = new TunnelMux(rec.socket, {
      onModelsReport: async () => {
        await new Promise((r) => setTimeout(r, 10))
        persisted = true
      },
    })
    const maybe = mux.handleMessage(frame({ t: "models", models: ["llama3"] }))
    expect(maybe).toBeInstanceOf(Promise)
    await maybe
    expect(persisted).toBe(true)
  })

  it("splits an oversized request body chunk into 1 MiB frames", async () => {
    const rec = recordingSocket()
    const mux = new TunnelMux(rec.socket)
    const big = new Uint8Array(2 * 1024 * 1024 + 5)
    const body = new Blob([big]).stream()
    const promise = mux.openRequest({ method: "POST", path: "/chat/completions", headers: {}, body })
    await flush()
    await flush()
    const sizes = rec.binaries().map((b) => b.chunk.byteLength)
    expect(Math.max(...sizes)).toBeLessThanOrEqual(1024 * 1024)
    expect(sizes.reduce((a, b) => a + b, 0)).toBe(big.byteLength)
    mux.handleMessage(frame({ t: "res_err", id: 1, reason: "aborted" }))
    await promise
  })
})

describe("agentFaultResponse", () => {
  it("carries the fault marker and a JSON body", async () => {
    const res = agentFaultResponse("offline")
    expect(res.status).toBe(502)
    expect(res.headers.get("x-agent-fault")).toBe("offline")
    expect(await res.json()).toEqual({ error: { type: "agent_fault", reason: "offline" } })
  })
})
