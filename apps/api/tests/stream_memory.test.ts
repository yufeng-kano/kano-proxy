import { describe, expect, it, vi } from "vitest"
import { Hono } from "hono"
import { readSseLines, SseLineTooLargeError } from "../src/proxy/sse_lines"
import { readProxyJson } from "../src/proxy/request_json"
import { rewriteOpenAIErrorFramesToResponses, openaiSseToResponsesStream } from "../src/proxy/responses_openai"
import { codexSseToOpenAIStream } from "../src/proxy/codex_openai"
import { grokResponsesSseToAnthropicStream } from "../src/proxy/grok_anthropic"
import { openaiSseToAnthropicStream, anthropicSseToOpenAIStream } from "../src/proxy/openai_anthropic"
import { MAX_PROXY_ERROR_BYTES } from "../src/proxy/sse_error_rewrite"

const encoder = new TextEncoder()
function bytes(text: string, size = 1): ReadableStream<Uint8Array> {
  const encoded = encoder.encode(text)
  let offset = 0
  return new ReadableStream({
    pull(controller) {
      if (offset >= encoded.length) { controller.close(); return }
      controller.enqueue(encoded.subarray(offset, offset + size))
      offset += size
    },
  })
}
async function text(body: ReadableStream<Uint8Array>): Promise<string> {
  return new Response(body).text()
}

describe("SSE line memory bounds", () => {
  it("decodes split UTF-8, CRLF, empty lines and an unterminated final line", async () => {
    const actual = []
    for await (const line of readSseLines(bytes('data: 台灣🙂\r\n\ndata: 結束'))) actual.push(line)
    expect(actual).toEqual(['data: 台灣🙂\r', '', 'data: 結束'])
  })

  it("enforces byte size even for a complete line in a single read and cancels the source", async () => {
    const cancel = vi.fn()
    const source = new ReadableStream<Uint8Array>({
      start(c) { c.enqueue(encoder.encode('台灣🙂\n')) },
      cancel,
    })
    const reader = readSseLines(source, 9)
    await expect(reader.next()).rejects.toBeInstanceOf(SseLineTooLargeError)
    expect(cancel).toHaveBeenCalledOnce()
    expect(source.locked).toBe(false)
  })

  it("allows an exact byte boundary and releases the lock after EOF", async () => {
    const source = bytes('台灣🙂\n', 2)
    const output = []
    for await (const line of readSseLines(source, 10)) output.push(line)
    expect(output).toEqual(['台灣🙂'])
    expect(source.locked).toBe(false)
  })

  it("cancels on an early consumer return instead of draining the upstream", async () => {
    const cancel = vi.fn()
    let pulls = 0
    const source = new ReadableStream<Uint8Array>({
      pull(c) { pulls++; c.enqueue(encoder.encode('data: next\n')) }, cancel,
    })
    for await (const _line of readSseLines(source)) break
    expect(cancel).toHaveBeenCalledOnce()
    expect(pulls).toBeLessThanOrEqual(2)
  })
})

describe("native Responses error rewriting", () => {
  it("forwards a 48 MiB ordinary line before its delimiter arrives", async () => {
    let remaining = 48 * 1024 * 1024
    const block = new Uint8Array(64 * 1024).fill(65)
    const prefix = encoder.encode('data: {"type":"response.completed","payload":"')
    const suffix = encoder.encode('"}\n\n')
    let phase = 0
    let pulledBytes = 0
    const source = new ReadableStream<Uint8Array>({
      pull(c) {
        let value: Uint8Array
        if (phase === 0) { phase = 1; value = prefix }
        else if (remaining) { remaining -= block.length; value = block }
        else if (phase === 1) { phase = 2; value = suffix }
        else { c.close(); return }
        pulledBytes += value.length
        c.enqueue(value)
      },
    })
    const reader = rewriteOpenAIErrorFramesToResponses(source, 'test').getReader()
    const first = await reader.read()
    expect(first.done).toBe(false)
    expect(pulledBytes).toBeLessThan(256 * 1024)
    let outputBytes = first.value!.length
    for (;;) {
      const next = await reader.read()
      if (next.done) break
      outputBytes += next.value.length
    }
    expect(outputBytes).toBe(prefix.length + 48 * 1024 * 1024 + suffix.length)
  })

  it.each([1, 2, 7, 13, 64])("preserves ordinary Unicode/CRLF/EOF bytes with %i-byte chunks", async size => {
    const original = 'event: response.created\r\ndata: {"type":"response.created","text":"台灣🙂"}\r\n\ndata: {"errors":[]}\n: comment\nlast'
    expect(await text(rewriteOpenAIErrorFramesToResponses(bytes(original, size), 'test'))).toBe(original)
  })

  it.each([1, 2, 7, 64])("rewrites split errors and resumes ordinary forwarding (%i-byte chunks)", async size => {
    const input = 'data:{"error":{"message":"台灣🙂","code":"request_too_large"}}\n\ndata: {"type":"unchanged"}\n\n'
    const result = await text(rewriteOpenAIErrorFramesToResponses(bytes(input, size), 'test'))
    expect(result).toContain('event: response.failed\ndata: ')
    expect(result).toContain('"message":"台灣🙂"')
    expect(result).toContain('"code":"request_too_large"')
    expect(result).toContain('\n\ndata: {"type":"unchanged"}\n\n')
  })

  it("bounds oversized proxy errors without leaking their payload or swallowing the next line", async () => {
    const input = 'data: {"error":{"message":"' + 'x'.repeat(MAX_PROXY_ERROR_BYTES + 1) + '"}}\n\ndata: {"type":"unchanged"}\n'
    const result = await text(rewriteOpenAIErrorFramesToResponses(bytes(input, 1024), 'test'))
    expect(result.length).toBeLessThan(1024)
    expect(result).toContain('response.failed')
    expect(result).toContain('64 KiB')
    expect(result).toContain('data: {"type":"unchanged"}')
  })

  it("propagates cancellation while a line is still arriving", async () => {
    const cancel = vi.fn()
    let pulls = 0
    const source = new ReadableStream<Uint8Array>({
      pull(c) { pulls++; c.enqueue(encoder.encode('data: normal')) }, cancel,
    })
    const reader = rewriteOpenAIErrorFramesToResponses(source, 'test').getReader()
    await reader.read()
    await reader.cancel('client left')
    await vi.waitFor(() => expect(cancel).toHaveBeenCalledOnce())
    expect(pulls).toBeLessThanOrEqual(4)
  })

  it("preserves malformed error-looking bytes instead of re-encoding them", async () => {
    const prefix = encoder.encode('data: {"error":')
    const original = new Uint8Array([...prefix, 255, 10])
    const source = new ReadableStream<Uint8Array>({ start(c) { c.enqueue(original); c.close() } })
    const actual = new Uint8Array(await new Response(rewriteOpenAIErrorFramesToResponses(source, 'test')).arrayBuffer())
    expect(actual).toEqual(original)
  })
})

const chatDelta = 'data: {"choices":[{"delta":{"content":"x"}}]}\n\n'
const codexDelta = 'data: {"type":"response.output_text.delta","delta":"x"}\n\n'
const converters: Array<[string, (body: ReadableStream<Uint8Array>) => ReadableStream<Uint8Array>, string]> = [
  ['codex', body => codexSseToOpenAIStream(body, 'test'), codexDelta],
  ['grok', body => grokResponsesSseToAnthropicStream(body, 'test', { thinkingMode: 'disabled' }), codexDelta],
  ['responses', body => openaiSseToResponsesStream(body, { model: 'test', toolNames: new Map() }), chatDelta],
  ['openai-to-anthropic', body => openaiSseToAnthropicStream(body, 'test'), chatDelta],
  ['anthropic-to-openai', body => anthropicSseToOpenAIStream(body, 'test'), 'event: content_block_delta\ndata: {"delta":{"type":"text_delta","text":"x"}}\n\n'],
]
describe.each(converters)("%s converter flow control", (_name, convert, delta) => {
  it("stops reading when the client pauses and cancels upstream when the client leaves", async () => {
    const cancel = vi.fn()
    let pulls = 0
    const source = new ReadableStream<Uint8Array>({
      pull(c) { pulls++; c.enqueue(encoder.encode(delta)) }, cancel,
    })
    const reader = convert(source).getReader()
    const first = await reader.read()
    expect(first.done).toBe(false)
    await new Promise(resolve => setTimeout(resolve, 10))
    const paused = pulls
    expect(paused).toBeLessThanOrEqual(4)
    await new Promise(resolve => setTimeout(resolve, 10))
    expect(pulls).toBe(paused)
    await reader.cancel('client left')
    await vi.waitFor(() => expect(cancel).toHaveBeenCalledOnce())
  })

  it("fails and cancels an oversized event rather than emitting a false completion", async () => {
    const cancel = vi.fn()
    let blocks = 0
    const oversized = new Uint8Array(64 * 1024).fill(65)
    const source = new ReadableStream<Uint8Array>({
      pull(c) { blocks++; c.enqueue(oversized) }, cancel,
    })
    await expect(text(convert(source))).rejects.toBeInstanceOf(SseLineTooLargeError)
    await vi.waitFor(() => expect(cancel).toHaveBeenCalledOnce())
    expect(blocks).toBeLessThanOrEqual(258)
  })
})

it("reuses parsed proxy JSON without retaining Hono's raw text cache", async () => {
  const app = new Hono()
  app.post('/', async c => {
    const body = await readProxyJson(c.req)
    expect(c.req.bodyCache.text).toBeUndefined()
    expect(await c.req.bodyCache.json).toBe(body)
    expect(await c.req.json()).toEqual(body)
    return c.json({ ok: true })
  })
  const res = await app.request('/', { method: 'POST', body: JSON.stringify({ input: 'x'.repeat(1024 * 1024) }) })
  expect(res.status).toBe(200)
})
