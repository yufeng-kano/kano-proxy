import type { DemandReader } from "./sse_lines"

/** Adapt an imperative converter without an eager, unbounded downstream queue. */
export function backpressuredStream(
  body: ReadableStream<Uint8Array>,
  pump: (reader: DemandReader, controller: ReadableStreamDefaultController<Uint8Array>) => Promise<void>,
): ReadableStream<Uint8Array> {
  const upstream = body.getReader()
  let cancelled = false
  let wake: (() => void) | undefined
  let controller: ReadableStreamDefaultController<Uint8Array>
  const ready = async () => {
    while (!cancelled && controller.desiredSize !== null && controller.desiredSize <= 0) {
      await new Promise<void>((resolve) => { wake = resolve })
    }
    if (cancelled) throw new Error("Stream cancelled")
  }
  return new ReadableStream<Uint8Array>({
    start(target) {
      controller = target
      const reader = {
        ready,
        async read() { await ready(); return upstream.read() },
        cancel: (reason?: unknown) => upstream.cancel(reason),
        releaseLock() { /* the pump's finalizer owns the real lock */ },
        get closed() { return upstream.closed },
      } as DemandReader
      const guarded = {
        get desiredSize() { return target.desiredSize },
        enqueue(value: Uint8Array) { if (!cancelled) target.enqueue(value) },
        close() { if (!cancelled) target.close() },
        error(reason: unknown) { if (!cancelled) target.error(reason) },
      } as ReadableStreamDefaultController<Uint8Array>
      // Do not return this promise from start: pull must run while the pump is active.
      void pump(reader, guarded).catch((error) => {
        if (!cancelled) target.error(error)
      }).finally(async () => {
        try { await upstream.cancel() } catch { /* already closed or failed */ }
        try { upstream.releaseLock() } catch { /* a pending cancellation owns cleanup */ }
      })
    },
    pull() { wake?.(); wake = undefined },
    cancel(reason) {
      cancelled = true
      wake?.(); wake = undefined
      return upstream.cancel(reason)
    },
  })
}
