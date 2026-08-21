import { describe, expect, it } from "vitest"
import { streamWithEagerProducer, streamWithKeepalive, type StreamCloseReason } from "../src/proxy/sse"

/** A source stream the test can push chunks into (or close) on demand. */
function controllableStream(): {
  stream: ReadableStream<Uint8Array>
  push: (text: string) => void
  close: () => void
} {
  let ctrl!: ReadableStreamDefaultController<Uint8Array>
  const stream = new ReadableStream<Uint8Array>({
    start(c) {
      ctrl = c
    },
  })
  return {
    stream,
    push: (text: string) => ctrl.enqueue(new TextEncoder().encode(text)),
    close: () => ctrl.close(),
  }
}

async function collect(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader()
  let out = ""
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    out += new TextDecoder().decode(value)
  }
  return out
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

describe("streamWithKeepalive — passthrough", () => {
  it("relays real bytes unmodified, in order, with no opts", async () => {
    const { stream, push, close } = controllableStream()
    const out = streamWithKeepalive(stream, 10_000)
    push("hello ")
    push("world")
    close()
    expect(await collect(out)).toBe("hello world")
  })
})

describe("streamWithKeepalive — keepalive re-arming", () => {
  it("re-arms the keepalive interval after a real chunk instead of stopping for the rest of the stream", async () => {
    const { stream, push, close } = controllableStream()
    const out = streamWithKeepalive(stream, 10) // 10ms keepalive interval
    // Consume live: the pump is demand-driven, so keepalives only flow while
    // a client is actually reading — an unread queue gets none (by design).
    const collected = collect(out)
    push("hello") // first real chunk
    await sleep(70) // several 10ms intervals elapse in the silence AFTER it
    close()
    const text = await collected
    expect(text).toContain("hello")
    const keepalives = (text.match(/: keepalive/g) ?? []).length
    // Old behavior stopped keepalives forever after the first byte (0 here).
    expect(keepalives).toBeGreaterThanOrEqual(2)
  })
})

describe("streamWithKeepalive — idle timeout", () => {
  it("emits the stall frame, closes the stream, and fires onClose('idle_timeout')", async () => {
    const { stream, push } = controllableStream()
    const reasons: StreamCloseReason[] = []
    const stallFrame = new TextEncoder().encode("STALL\n")
    const out = streamWithKeepalive(stream, 10_000, {
      idleTimeoutMs: 20,
      stallFrame,
      onClose: (reason) => reasons.push(reason),
    })
    push("hello") // one real chunk, then silence forever
    const text = await collect(out) // resolves once the idle timer closes the stream
    expect(text).toContain("hello")
    expect(text).toContain("STALL")
    expect(reasons).toEqual(["idle_timeout"])
  })

  it("never fires when real chunks keep arriving inside the idle window", async () => {
    const { stream, push, close } = controllableStream()
    const reasons: StreamCloseReason[] = []
    const out = streamWithKeepalive(stream, 10_000, {
      idleTimeoutMs: 40,
      stallFrame: new TextEncoder().encode("STALL\n"),
      onClose: (reason) => reasons.push(reason),
    })
    push("a")
    await sleep(20)
    push("b")
    await sleep(20)
    push("c")
    close()
    const text = await collect(out)
    expect(text).toBe("abc")
    expect(reasons).toEqual(["done"])
  })

  it("does not fire when unset (default disabled)", async () => {
    const { stream, push, close } = controllableStream()
    const reasons: StreamCloseReason[] = []
    const out = streamWithKeepalive(stream, 10_000, { onClose: (r) => reasons.push(r) })
    push("hello")
    await sleep(30)
    close()
    const text = await collect(out)
    expect(text).toBe("hello")
    expect(reasons).toEqual(["done"])
  })
})

describe("streamWithKeepalive — onClose reasons", () => {
  it("a clean upstream end fires onClose('done')", async () => {
    const { stream, push, close } = controllableStream()
    const reasons: StreamCloseReason[] = []
    const out = streamWithKeepalive(stream, 10_000, { onClose: (r) => reasons.push(r) })
    push("hi")
    close()
    await collect(out)
    expect(reasons).toEqual(["done"])
  })

  it("the client cancelling the outgoing stream fires onClose('cancel')", async () => {
    const { stream, push } = controllableStream()
    const reasons: StreamCloseReason[] = []
    const out = streamWithKeepalive(stream, 10_000, { onClose: (r) => reasons.push(r) })
    push("hi")
    const reader = out.getReader()
    await reader.read()
    await reader.cancel()
    expect(reasons).toEqual(["cancel"])
  })

  it("onClose fires exactly once even if both cancel and natural close could race", async () => {
    const { stream, push, close } = controllableStream()
    const reasons: StreamCloseReason[] = []
    const out = streamWithKeepalive(stream, 10_000, { onClose: (r) => reasons.push(r) })
    push("hi")
    close()
    const reader = out.getReader()
    await reader.read()
    await reader.read() // done: true
    await reader.cancel().catch(() => {})
    expect(reasons).toHaveLength(1)
  })
})

describe("streamWithKeepalive — tap", () => {
  it("tap sees every real chunk, never the keepalive comments", async () => {
    const { stream, push, close } = controllableStream()
    const seen: string[] = []
    const out = streamWithKeepalive(stream, 10, {
      tap: (chunk) => seen.push(new TextDecoder().decode(chunk)),
    })
    push("chunk-1")
    await sleep(30)
    push("chunk-2")
    close()
    await collect(out)
    expect(seen).toEqual(["chunk-1", "chunk-2"])
  })
})


describe("streamWithEagerProducer", () => {
  it("keepalives fire before pipeUpstream", async () => {
    let release!: () => void
    const gate = new Promise<void>((resolve) => {
      release = resolve
    })
    const reasons: StreamCloseReason[] = []
    const out = streamWithEagerProducer(
      async (ctl) => {
        await gate
        ctl.close()
      },
      15,
      { onClose: (r) => reasons.push(r) },
    )
    const reader = out.getReader()
    // Wait long enough for at least one keepalive while still blocked in run.
    await sleep(45)
    release()
    let text = ""
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      text += new TextDecoder().decode(value)
    }
    expect(text).toContain(": keepalive")
    expect(reasons).toEqual(["done"])
  })

  it("pipeUpstream relays bytes unmodified", async () => {
    const { stream, push, close } = controllableStream()
    const out = streamWithEagerProducer(async (ctl) => {
      await ctl.pipeUpstream(stream)
    }, 10_000)
    push("hello ")
    push("world")
    close()
    expect(await collect(out)).toBe("hello world")
  })

  it("fail enqueues the frame and closes", async () => {
    const reasons: StreamCloseReason[] = []
    const frame = new TextEncoder().encode("ERR\n")
    const out = streamWithEagerProducer(
      async (ctl) => {
        ctl.fail(frame)
      },
      10_000,
      { onClose: (r) => reasons.push(r) },
    )
    expect(await collect(out)).toBe("ERR\n")
    // Terminal in-stream error is a clean end from the pipe's view.
    expect(reasons).toEqual(["done"])
  })

  it("cancel during wait → onClose cancel", async () => {
    const reasons: StreamCloseReason[] = []
    const out = streamWithEagerProducer(
      async () => {
        await new Promise(() => {}) // hang forever
      },
      10_000,
      { onClose: (r) => reasons.push(r) },
    )
    const reader = out.getReader()
    await sleep(5)
    await reader.cancel()
    expect(reasons).toEqual(["cancel"])
  })

  it("idle timeout only arms after pipeUpstream starts", async () => {
    let release!: () => void
    const prePipe = new Promise<void>((resolve) => {
      release = resolve
    })
    const reasons: StreamCloseReason[] = []
    const stallFrame = new TextEncoder().encode("STALL\n")
    const out = streamWithEagerProducer(
      async (ctl) => {
        // Spend longer than idleTimeoutMs before piping — must NOT stall yet.
        await prePipe
        const { stream, push } = controllableStream()
        // Start pipe with one chunk then silence so idle can fire during pipe.
        const pipePromise = ctl.pipeUpstream(stream, {
          idleTimeoutMs: 25,
          stallFrame,
        })
        push("first")
        await pipePromise
      },
      10_000,
      { onClose: (r) => reasons.push(r) },
    )

    // Hold pre-pipe for > idle timeout window; stream should stay open.
    await sleep(50)
    release()
    const text = await collect(out)
    expect(text).toContain("first")
    expect(text).toContain("STALL")
    expect(reasons).toEqual(["idle_timeout"])
  })
})

describe("backpressure — demand-driven pumps", () => {
  /** Upstream that serves a counted frame per pull, up to `total`. */
  function countingUpstream(total: number): {
    stream: ReadableStream<Uint8Array>
    served: () => number
  } {
    let served = 0
    const encoder = new TextEncoder()
    const stream = new ReadableStream<Uint8Array>({
      pull(controller) {
        if (served >= total) {
          controller.close()
          return
        }
        served++
        controller.enqueue(encoder.encode(`chunk-${served}\n`))
      },
    })
    return { stream, served: () => served }
  }

  it("streamWithKeepalive does not drain upstream ahead of client demand", async () => {
    const { stream, served } = countingUpstream(20)
    const reader = streamWithKeepalive(stream, 10_000).getReader()
    await reader.read()
    // Let any stray eager pumping run — the pump must be parked on demand,
    // not buffering the remaining upstream into the wrapper queue.
    await sleep(10)
    expect(served()).toBeLessThan(6)
    await reader.cancel()
  })

  it("pipeUpstream does not drain upstream ahead of client demand", async () => {
    const { stream, served } = countingUpstream(20)
    const out = streamWithEagerProducer(async (ctl) => {
      await ctl.pipeUpstream(stream)
    }, 10_000)
    const reader = out.getReader()
    await reader.read()
    await sleep(10)
    expect(served()).toBeLessThan(6)
    await reader.cancel()
  })
})
