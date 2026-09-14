const PREFIXES = [new TextEncoder().encode('data: {"error"'), new TextEncoder().encode('data:{"error"')]
export const MAX_PROXY_ERROR_BYTES = 64 * 1024

/** Inspect a short prefix, stream ordinary lines untouched, and buffer only small proxy errors. */
export function rewriteSseErrors(
  source: ReadableStream<Uint8Array>,
  rewrite: (line: string) => string,
): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder()
  const decoder = new TextDecoder()
  const prefix = new Uint8Array(Math.max(...PREFIXES.map(p => p.length)))
  let prefixSize = 0
  let state: "prefix" | "pass" | "error" | "skip" = "prefix"
  let error: Uint8Array | undefined
  let errorSize = 0
  const reset = () => { state = "prefix"; prefixSize = 0; error = undefined; errorSize = 0 }
  const endLine = (controller: TransformStreamDefaultController<Uint8Array>, newline: boolean) => {
    if (state === "prefix" && prefixSize) controller.enqueue(prefix.slice(0, prefixSize))
    if (state === "error") {
      const bytes = error!.subarray(0, errorSize)
      const original = decoder.decode(bytes)
      const rewritten = rewrite(original)
      if (rewritten === original) {
        controller.enqueue(bytes)
        if (newline) controller.enqueue(new Uint8Array([10]))
      } else controller.enqueue(encoder.encode(rewritten + (newline ? "\n" : "")))
    }
    reset()
  }
  return source.pipeThrough(new TransformStream<Uint8Array, Uint8Array>({
    transform(chunk, controller) {
      let offset = 0
      while (offset < chunk.length) {
        const newline = chunk.indexOf(10, offset)
        const end = newline < 0 ? chunk.length : newline
        while (offset < end && state === "prefix") {
          prefix[prefixSize++] = chunk[offset++]!
          const candidates = PREFIXES.filter(p => prefixSize <= p.length && prefix.subarray(0, prefixSize).every((b, i) => b === p[i]))
          if (!candidates.length) {
            state = "pass"
            controller.enqueue(prefix.slice(0, prefixSize))
          } else if (candidates.some(p => p.length === prefixSize)) {
            state = "error"
            error = new Uint8Array(MAX_PROXY_ERROR_BYTES)
            error.set(prefix.subarray(0, prefixSize))
            errorSize = prefixSize
          }
        }
        if (state === "pass") {
          // Include the delimiter without decoding/re-encoding normal bytes.
          if (end > offset || newline >= 0) controller.enqueue(chunk.subarray(offset, end + (newline >= 0 ? 1 : 0)))
        } else if (state === "error") {
          if (errorSize + end - offset > MAX_PROXY_ERROR_BYTES) {
            controller.enqueue(encoder.encode(rewrite('data: {"error":{"code":"upstream_error","message":"Proxy error exceeds the 64 KiB rewrite limit"}}') + "\n"))
            error = undefined
            state = "skip"
          } else {
            error!.set(chunk.subarray(offset, end), errorSize)
            errorSize += end - offset
          }
        }
        if (newline >= 0) {
          if (state === "prefix") {
            if (prefixSize) controller.enqueue(prefix.slice(0, prefixSize))
            controller.enqueue(chunk.subarray(newline, newline + 1))
            reset()
          } else endLine(controller, true)
        }
        offset = end + (newline >= 0 ? 1 : 0)
      }
    },
    flush(controller) { endLine(controller, false) },
  }))
}
