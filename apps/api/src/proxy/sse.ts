/** SSE helpers — never buffer entire upstream streams. */

export function sseKeepaliveComment(): Uint8Array {
  return new TextEncoder().encode(": keepalive\n\n")
}

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
   * error, or because the client cancelled/aborted mid-stream — so a
   * caller can finish up with whatever the tap saw. Wrapped in try/catch.
   */
  onClose?: () => void
}

/**
 * Relay an upstream body, injecting SSE comment keepalives until first byte
 * (Cloudflare idle timeout mitigation). `opts.tap`/`opts.onClose` observe
 * the passthrough without altering it — emitted bytes and timing are
 * byte-identical to calling this with no opts.
 */
export function streamWithKeepalive(
  upstream: ReadableStream<Uint8Array>,
  intervalMs = 30_000,
  opts?: StreamKeepaliveOpts,
): ReadableStream<Uint8Array> {
  const reader = upstream.getReader()
  let closed = false
  let closeFired = false
  const fireClose = () => {
    if (closeFired) return
    closeFired = true
    try {
      opts?.onClose?.()
    } catch {
      /* capture must never break the stream */
    }
  }
  return new ReadableStream<Uint8Array>({
    async start(controller) {
      let timer: ReturnType<typeof setInterval> | undefined
      const arm = () => {
        timer = setInterval(() => {
          try {
            controller.enqueue(sseKeepaliveComment())
          } catch {
            /* closed */
          }
        }, intervalMs)
      }
      arm()
      try {
        for (;;) {
          const { done, value } = await reader.read()
          if (done) break
          if (timer) {
            clearInterval(timer)
            timer = undefined
          }
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
        }
        controller.close()
        fireClose()
      } catch (e) {
        controller.error(e)
        fireClose()
      } finally {
        if (timer) clearInterval(timer)
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
      reader.cancel().catch(() => {})
      fireClose()
    },
  })
}
