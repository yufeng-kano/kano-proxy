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
  intervalMs = 30_000,
  opts?: StreamKeepaliveOpts,
): ReadableStream<Uint8Array> {
  const reader = upstream.getReader()
  let closed = false
  let closeFired = false
  let keepaliveTimer: ReturnType<typeof setInterval> | undefined
  let idleTimer: ReturnType<typeof setTimeout> | undefined

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

  return new ReadableStream<Uint8Array>({
    async start(controller) {
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
          fireClose("idle_timeout")
        }, opts.idleTimeoutMs)
      }

      armKeepalive()
      armIdle()
      try {
        for (;;) {
          const { done, value } = await reader.read()
          if (idleTimedOut) break
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
        if (!idleTimedOut) {
          controller.close()
          fireClose("done")
        }
      } catch (e) {
        if (!idleTimedOut) {
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
    },
    cancel() {
      closed = true
      clearKeepalive()
      clearIdle()
      reader.cancel().catch(() => {})
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
    pipeOpts?: Pick<StreamKeepaliveOpts, "tap" | "idleTimeoutMs" | "stallFrame">,
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
  intervalMs = 30_000,
  opts?: StreamKeepaliveOpts,
): ReadableStream<Uint8Array> {
  let closed = false
  let closeFired = false
  let cancelled = false
  let keepaliveTimer: ReturnType<typeof setInterval> | undefined
  let idleTimer: ReturnType<typeof setTimeout> | undefined
  let upstreamReader: ReadableStreamDefaultReader<Uint8Array> | null = null

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

  return new ReadableStream<Uint8Array>({
    async start(controller) {
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
              fireClose("idle_timeout")
            }, pipeOpts.idleTimeoutMs)
          }

          // Re-arm keepalive for the piped phase; idle starts only now.
          armKeepalive()
          armIdle()

          try {
            for (;;) {
              if (cancelled || closed) break
              const { done, value } = await reader.read()
              if (idleTimedOut || closed) break
              clearKeepalive()
              clearIdle()
              if (done) break
              if (value) {
                try {
                  controller.enqueue(value)
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
              closed = true
              clearKeepalive()
              clearIdle()
              try {
                controller.error(e)
              } catch {
                /* */
              }
              fireClose("error")
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
      } catch (e) {
        if (!closed) {
          closed = true
          clearKeepalive()
          clearIdle()
          try {
            controller.error(e)
          } catch {
            /* */
          }
          fireClose("error")
        }
      }
    },
    cancel() {
      cancelled = true
      clearKeepalive()
      clearIdle()
      if (upstreamReader) {
        upstreamReader.cancel().catch(() => {})
      }
      closed = true
      fireClose("cancel")
    },
  })
}
