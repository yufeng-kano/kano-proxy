/** SSE helpers — never buffer entire upstream streams. */

export function sseKeepaliveComment(): Uint8Array {
  return new TextEncoder().encode(": keepalive\n\n")
}

/**
 * Relay an upstream body, injecting SSE comment keepalives until first byte
 * (Cloudflare idle timeout mitigation).
 */
export function streamWithKeepalive(
  upstream: ReadableStream<Uint8Array>,
  intervalMs = 30_000,
): ReadableStream<Uint8Array> {
  const reader = upstream.getReader()
  let closed = false
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
          if (value) controller.enqueue(value)
        }
        controller.close()
      } catch (e) {
        controller.error(e)
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
    },
  })
}
