/** SSE helpers — never buffer entire upstream streams. */

export function sseKeepaliveComment(): Uint8Array {
  return new TextEncoder().encode(": keepalive\n\n")
}

/** Why the outgoing stream ended — passed to `onClose` for logging (docs/logging.md "Streaming rows"). */
export type StreamCloseReason = "done" | "cancel" | "error" | "idle_timeout"

export type StreamKeepaliveOpts = {
  /**
   * Called with every real upstream chunk, in order, right after it is
   * enqueued to the outgoing stream — never for the keepalive comments.
   * Wrapped in try/catch: a tap failure degrades capture, it never breaks
   * the stream or changes what bytes reach the client.
   */
  tap?: (chunk: Uint8Array) => void
  /**
   * Called exactly once when the stream ends — naturally, on upstream
   * error, because the client cancelled/aborted mid-stream, or because
   * `idleTimeoutMs` elapsed with no upstream chunk — so a caller can finish
   * up with whatever the tap saw. Wrapped in try/catch.
   */
  onClose?: (reason: StreamCloseReason) => void
  /**
   * No real upstream chunk (keepalive comments never count) for this many
   * ms tears the connection down: enqueue `stallFrame` if given, stop
   * reading upstream, close the outgoing stream cleanly, and fire
   * `onClose("idle_timeout")`. Unset/0 disables this entirely (the default).
   */
  idleTimeoutMs?: number
  /** Enqueued once, immediately before close, when `idleTimeoutMs` fires. */
  stallFrame?: Uint8Array
  /** Terminal frame for errors before any real upstream bytes were emitted. */
  errorFrame?: Uint8Array
}

/**
 * Relay an upstream body, injecting SSE comment keepalives across every
 * silence gap — not just before the first byte — for the whole stream
 * lifetime (Cloudflare idle-connection mitigation). `opts.tap`/`opts.onClose`
 * observe the passthrough without altering it: emitted upstream bytes and
 * their relative order are byte-identical to calling this with no opts.
 * Keepalive comments and an optional idle `stallFrame` are the only bytes
 * this function itself ever adds.
 */
export function streamWithKeepalive(
  upstream: ReadableStream<Uint8Array>,
  intervalMs = 10_000,
  opts?: StreamKeepaliveOpts,
): ReadableStream<Uint8Array> {
  const reader = upstream.getReader()
  let closed = false
  let closeFired = false
  let keepaliveTimer: ReturnType<typeof setInterval> | undefined
  let idleTimer: ReturnType<typeof setTimeout> | undefined
  /** Resolved by `pull()` — the pump parks on this while the client is not consuming. */
  let demandWaiter: (() => void) | null = null

  const signalDemand = () => {
    const waiter = demandWaiter
    demandWaiter = null
    waiter?.()
  }
  const clearKeepalive = () => {
    if (keepaliveTimer) {
      clearInterval(keepaliveTimer)
      keepaliveTimer = undefined
    }
  }
  const clearIdle = () => {
    if (idleTimer) {
      clearTimeout(idleTimer)
      idleTimer = undefined
    }
  }
  const fireClose = (reason: StreamCloseReason) => {
    if (closeFired) return
    closeFired = true
    try {
      opts?.onClose?.(reason)
    } catch {
      /* capture must never break the stream */
    }
  }

  // The pump runs detached from start() — pull() is never invoked while
  // start()'s promise is pending, so a pump awaited inside start() could
  // never observe downstream demand and would buffer the whole upstream.
  const pump = async (controller: ReadableStreamDefaultController<Uint8Array>) => {
    let idleTimedOut = false
    const armKeepalive = () => {
      keepaliveTimer = setInterval(() => {
        try {
          controller.enqueue(sseKeepaliveComment())
        } catch {
          /* closed */
        }
      }, intervalMs)
    }
    const armIdle = () => {
      if (!opts?.idleTimeoutMs) return
      idleTimer = setTimeout(() => {
        if (closeFired) return
        idleTimedOut = true
        clearKeepalive()
        try {
          if (opts?.stallFrame) controller.enqueue(opts.stallFrame)
        } catch {
          /* closed */
        }
        try {
          controller.close()
        } catch {
          /* already closed */
        }
        closed = true
        reader.cancel().catch(() => {})
        signalDemand()
        fireClose("idle_timeout")
      }, opts.idleTimeoutMs)
    }

    armKeepalive()
    armIdle()
    try {
      for (;;) {
        // Demand gate: while the client is not consuming, stop reading
        // upstream so backpressure propagates instead of the remaining
        // generation buffering here. Timers pause with the pump — a client
        // stall is not an upstream silence gap.
        if (!closed && controller.desiredSize !== null && controller.desiredSize <= 0) {
          clearKeepalive()
          clearIdle()
          await new Promise<void>((resolve) => {
            demandWaiter = resolve
          })
          if (closed || idleTimedOut) break
          armKeepalive()
          armIdle()
        }
        if (closed) break
        const { done, value } = await reader.read()
        if (idleTimedOut || closed) break
        // The wait is over — the silence gap ends here whether this was a
        // real chunk or natural EOF, so both timers reset.
        clearKeepalive()
        clearIdle()
        if (done) break
        if (value) {
          controller.enqueue(value)
          if (opts?.tap) {
            try {
              opts.tap(value)
            } catch {
              /* capture must never break the stream */
            }
          }
        }
        // Re-arm for the next gap — keepalive and idle timeout both cover
        // the whole stream lifetime, not just the time before first byte.
        armKeepalive()
        armIdle()
      }
      if (!idleTimedOut && !closed) {
        controller.close()
        fireClose("done")
      }
    } catch (e) {
      if (!idleTimedOut && !closed) {
        controller.error(e)
        fireClose("error")
      }
    } finally {
      clearKeepalive()
      clearIdle()
      if (!closed) {
        closed = true
        try {
          reader.releaseLock()
        } catch {
          /* */
        }
      }
    }
  }

  return new ReadableStream<Uint8Array>({
    start(controller) {
      void pump(controller)
    },
    pull() {
      signalDemand()
    },
    cancel() {
      closed = true
      clearKeepalive()
      clearIdle()
      reader.cancel().catch(() => {})
      signalDemand()
      fireClose("cancel")
    },
  })
}


/**
 * Controller handed to `streamWithEagerProducer`'s `run` callback.
 * Keepalives already fire from stream start; idle timeout arms only inside
 * `pipeUpstream` (docs/api.md "Eager streaming commit").
 */
export type EagerStreamController = {
  /** Client already cancelled — stop acquire/upstream work. */
  cancelled: () => boolean
  /**
   * Relay an upstream body with the same keepalive re-arm + optional idle
   * timeout rules as `streamWithKeepalive`. Resolves when the upstream ends,
   * idle-timeout fires, or the client cancels. Idle timeout starts only here
   * — not during the pre-upstream TTFB wait.
   */
  pipeUpstream: (
    upstream: ReadableStream<Uint8Array>,
    pipeOpts?: Pick<StreamKeepaliveOpts, "tap" | "idleTimeoutMs" | "stallFrame" | "errorFrame">,
  ) => Promise<void>
  /** Enqueue one terminal frame (error) and close cleanly. */
  fail: (frame: Uint8Array) => void
  /** Close cleanly with no extra frame. */
  close: () => void
}

/**
 * SSE stream that commits immediately: keepalives from second 0, while
 * `run` performs acquire / failover / upstream fetch and either pipes a
 * body or emits a terminal error frame. Used for client `stream: true`.
 */
export function streamWithEagerProducer(
  run: (ctl: EagerStreamController) => Promise<void>,
  intervalMs = 10_000,
  opts?: StreamKeepaliveOpts,
): ReadableStream<Uint8Array> {
  let closed = false
  let closeFired = false
  let cancelled = false
  let keepaliveTimer: ReturnType<typeof setInterval> | undefined
  let idleTimer: ReturnType<typeof setTimeout> | undefined
  let upstreamReader: ReadableStreamDefaultReader<Uint8Array> | null = null
  /** Resolved by `pull()` — `pipeUpstream` parks on this while the client is not consuming. */
  let demandWaiter: (() => void) | null = null

  const signalDemand = () => {
    const waiter = demandWaiter
    demandWaiter = null
    waiter?.()
  }
  const clearKeepalive = () => {
    if (keepaliveTimer) {
      clearInterval(keepaliveTimer)
      keepaliveTimer = undefined
    }
  }
  const clearIdle = () => {
    if (idleTimer) {
      clearTimeout(idleTimer)
      idleTimer = undefined
    }
  }
  const fireClose = (reason: StreamCloseReason) => {
    if (closeFired) return
    closeFired = true
    try {
      opts?.onClose?.(reason)
    } catch {
      /* capture must never break the stream */
    }
  }

  // The producer runs detached from start() — pull() is never invoked while
  // start()'s promise is pending, so awaiting `run` inside start() would make
  // `pipeUpstream` blind to downstream demand and buffer the whole upstream.
  const produce = async (controller: ReadableStreamDefaultController<Uint8Array>) => {
      const armKeepalive = () => {
        clearKeepalive()
        keepaliveTimer = setInterval(() => {
          try {
            controller.enqueue(sseKeepaliveComment())
          } catch {
            /* closed */
          }
        }, intervalMs)
      }

      // Keepalive from second 0 — before any upstream body exists.
      armKeepalive()

      const safeClose = (reason: StreamCloseReason) => {
        if (closed) return
        closed = true
        clearKeepalive()
        clearIdle()
        try {
          controller.close()
        } catch {
          /* already closed */
        }
        fireClose(reason)
      }

      const ctl: EagerStreamController = {
        cancelled: () => cancelled,
        fail: (frame) => {
          if (closed) return
          clearKeepalive()
          clearIdle()
          try {
            controller.enqueue(frame)
          } catch {
            /* closed */
          }
          closed = true
          try {
            controller.close()
          } catch {
            /* */
          }
          // Terminal in-stream error is a clean end from the pipe's view;
          // callers force `error_code` via their own state for logging.
          fireClose("done")
        },
        close: () => safeClose(cancelled ? "cancel" : "done"),
        pipeUpstream: async (upstream, pipeOpts) => {
          if (closed || cancelled) {
            try {
              await upstream.cancel()
            } catch {
              /* */
            }
            return
          }
          const reader = upstream.getReader()
          upstreamReader = reader
          let idleTimedOut = false
          let emittedUpstreamBytes = false

          const armIdle = () => {
            if (!pipeOpts?.idleTimeoutMs) return
            clearIdle()
            idleTimer = setTimeout(() => {
              if (closeFired) return
              idleTimedOut = true
              clearKeepalive()
              clearIdle()
              try {
                if (pipeOpts?.stallFrame) controller.enqueue(pipeOpts.stallFrame)
              } catch {
                /* closed */
              }
              try {
                controller.close()
              } catch {
                /* */
              }
              closed = true
              reader.cancel().catch(() => {})
              signalDemand()
              fireClose("idle_timeout")
            }, pipeOpts.idleTimeoutMs)
          }

          // Re-arm keepalive for the piped phase; idle starts only now.
          armKeepalive()
          armIdle()

          try {
            for (;;) {
              if (cancelled || closed) break
              // Demand gate: while the client is not consuming, stop reading
              // upstream so backpressure propagates instead of the remaining
              // generation buffering here. Timers pause with the pump — a
              // client stall is not an upstream silence gap.
              if (controller.desiredSize !== null && controller.desiredSize <= 0) {
                clearKeepalive()
                clearIdle()
                await new Promise<void>((resolve) => {
                  demandWaiter = resolve
                })
                if (cancelled || closed || idleTimedOut) break
                armKeepalive()
                armIdle()
              }
              const { done, value } = await reader.read()
              if (idleTimedOut || closed) break
              clearKeepalive()
              clearIdle()
              if (done) break
              if (value) {
                try {
                  controller.enqueue(value)
                  emittedUpstreamBytes = true
                } catch {
                  /* client gone */
                  break
                }
                if (pipeOpts?.tap) {
                  try {
                    pipeOpts.tap(value)
                  } catch {
                    /* capture must never break the stream */
                  }
                }
                if (opts?.tap) {
                  try {
                    opts.tap(value)
                  } catch {
                    /* */
                  }
                }
              }
              if (cancelled || closed) break
              armKeepalive()
              armIdle()
            }
            if (!idleTimedOut && !closed) {
              safeClose(cancelled ? "cancel" : "done")
            }
          } catch (e) {
            if (!idleTimedOut && !closed) {
              clearKeepalive()
              clearIdle()
              // Once a real byte has flowed, a stream may be mid-frame and raw
              // abort is the only honest option. Before output, preserve the
              // client protocol with the dispatch-provided terminal frame.
              if (!emittedUpstreamBytes && pipeOpts?.errorFrame) {
                try {
                  controller.enqueue(pipeOpts.errorFrame)
                  controller.close()
                  closed = true
                  fireClose("done")
                } catch {
                  closed = true
                  fireClose("error")
                }
              } else {
                closed = true
                try {
                  controller.error(e)
                } catch {
                  /* */
                }
                fireClose("error")
              }
            }
          } finally {
            clearKeepalive()
            clearIdle()
            upstreamReader = null
            try {
              reader.releaseLock()
            } catch {
              /* */
            }
          }
        },
      }

      try {
        await run(ctl)
        if (!closed) {
          safeClose(cancelled ? "cancel" : "done")
        }
      } catch {
        if (!closed) {
          clearKeepalive()
          clearIdle()
          // Eager dispatch has not handed us upstream bytes at this level:
          // always preserve SSE protocol with a generic terminal frame.
          try {
            controller.enqueue(opts?.errorFrame ?? new TextEncoder().encode('data: {"error":{"message":"upstream error","type":"api_error"}}\n\n'))
            controller.close()
            closed = true
            fireClose("done")
          } catch {
            closed = true
            fireClose("error")
          }
        }
      }
  }

  return new ReadableStream<Uint8Array>({
    start(controller) {
      void produce(controller)
    },
    pull() {
      signalDemand()
    },
    cancel() {
      cancelled = true
      clearKeepalive()
      clearIdle()
      if (upstreamReader) {
        upstreamReader.cancel().catch(() => {})
      }
      closed = true
      signalDemand()
      fireClose("cancel")
    },
  })
}
