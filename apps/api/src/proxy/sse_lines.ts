/** Parsed SSE lines have a finite byte budget; native byte passthrough does not use this reader. */
export const MAX_SSE_LINE_BYTES = 16 * 1024 * 1024

export class SseLineTooLargeError extends Error {
  constructor() {
    super("Upstream SSE event exceeds the 16 MiB parsing limit")
    this.name = "SseLineTooLargeError"
  }
}

export type DemandReader = ReadableStreamDefaultReader<Uint8Array> & { ready?: () => Promise<void> }

/** Each byte is scanned once and each line joined once, including unterminated EOF lines. */
export async function* readSseLines(
  source: ReadableStream<Uint8Array> | DemandReader,
  maxBytes = MAX_SSE_LINE_BYTES,
): AsyncGenerator<string> {
  const reader: DemandReader = "getReader" in source ? source.getReader() : source
  let parts: string[] = []
  let fragment = ""
  let bytes = 0
  const decoder = new TextDecoder()
  let eof = false
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) { eof = true; break }
      let start = 0
      while (start < value.length) {
        const newline = value.indexOf(10, start)
        const end = newline === -1 ? value.length : newline
        bytes += end - start
        if (bytes > maxBytes) throw new SseLineTooLargeError()
        const text = decoder.decode(value.subarray(start, end), { stream: newline === -1 })
        fragment += text
        if (fragment.length >= 4096) { parts.push(fragment); fragment = "" }
        if (newline !== -1) {
          if (fragment) parts.push(fragment)
          const line = parts.join("")
          parts = []; fragment = ""; bytes = 0
          await reader.ready?.()
          yield line
        }
        start = newline === -1 ? value.length : newline + 1
      }
    }
    const tail = decoder.decode()
    if (fragment || tail) parts.push(fragment + tail)
    if (parts.length) {
      await reader.ready?.()
      yield parts.join("")
    }
  } finally {
    parts = []
    if (!eof) {
      try { await reader.cancel() } catch { /* preserve the original failure */ }
    }
    try { reader.releaseLock() } catch { /* owned by an outer stream */ }
  }
}
