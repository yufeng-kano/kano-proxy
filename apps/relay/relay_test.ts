/**
 * Zero real network anywhere in this file — every case stubs `fetchImpl`.
 * Mirrors docs/codex-relay.md "Testing and verification", item 1-4.
 *
 * No assertion library: this app is deliberately dependency-free (see
 * deno.json — "no imports map needed"), tests included.
 */

import { createRelayHandler } from "./relay.ts"

function deepEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true
  if (typeof a !== typeof b || a === null || b === null) return false
  if (typeof a !== "object") return false
  const aKeys = Object.keys(a as Record<string, unknown>)
  const bKeys = Object.keys(b as Record<string, unknown>)
  if (aKeys.length !== bKeys.length) return false
  return aKeys.every((k) =>
    deepEqual((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]),
  )
}

function assertEquals<T>(actual: T, expected: T, msg?: string): void {
  if (!deepEqual(actual, expected)) {
    throw new Error(
      msg ?? `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
    )
  }
}

function assert(condition: unknown, msg: string): void {
  if (!condition) throw new Error(msg)
}

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((r) => {
    resolve = r
  })
  return { promise, resolve }
}

const UPSTREAM = "https://upstream.example"

// ---------------------------------------------------------------------------
// (a) Header allowlist
// ---------------------------------------------------------------------------

Deno.test("forwards only the allowlisted headers and forces accept-encoding: identity", async () => {
  let captured: Headers | undefined
  const fetchImpl = (async (_url, init) => {
    captured = new Headers(init?.headers)
    return new Response("upstream ok")
  }) as typeof fetch

  const handler = createRelayHandler({ upstreamBase: UPSTREAM, fetchImpl })
  const req = new Request(`${UPSTREAM}/backend-api/codex/responses`, {
    method: "POST",
    headers: {
      "CF-Worker": "kano-proxy",
      "CF-Connecting-IP": "1.2.3.4",
      "CDN-Loop": "cloudflare",
      "CF-Ray": "abc123-SJC",
      "X-Serverless-Authorization": "Bearer fake-iam-token",
      "X-Forwarded-For": "5.6.7.8",
      Via: "1.1 cloudflare",
      cookie: "session=abc",
      authorization: "Bearer upstream-token",
      "chatgpt-account-id": "acct_1",
      session_id: "sess_1",
      originator: "codex_cli_rs",
      "user-agent": "codex-tui/1.0",
      "content-type": "application/json",
      accept: "text/event-stream",
      "accept-language": "en-US,en;q=0.9",
      "openai-beta": "responses=experimental",
    },
    body: "{}",
  })

  await handler(req)
  assert(captured, "fetchImpl was never called")

  const forwarded: Record<string, string> = {
    authorization: "Bearer upstream-token",
    "chatgpt-account-id": "acct_1",
    session_id: "sess_1",
    originator: "codex_cli_rs",
    "user-agent": "codex-tui/1.0",
    "content-type": "application/json",
    accept: "text/event-stream",
    "accept-language": "en-US,en;q=0.9",
    "openai-beta": "responses=experimental",
  }
  for (const [name, value] of Object.entries(forwarded)) {
    assertEquals(captured!.get(name), value, `header ${name}`)
  }
  assertEquals(captured!.get("accept-encoding"), "identity")

  const poison = [
    "cf-worker",
    "cf-connecting-ip",
    "cdn-loop",
    "cf-ray",
    "x-serverless-authorization",
    "x-forwarded-for",
    "via",
    "cookie",
  ]
  for (const name of poison) {
    assert(!captured!.has(name), `poison header ${name} reached upstream`)
  }
})

// ---------------------------------------------------------------------------
// (b) No-buffering proof
// ---------------------------------------------------------------------------

Deno.test("streams the response body through without buffering", async () => {
  const releaseB = deferred<void>()
  const upstreamBody = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode("A"))
      // Held open deliberately — "B" only arrives once the test releases it.
      releaseB.promise.then(() => {
        controller.enqueue(new TextEncoder().encode("B"))
        controller.close()
      })
    },
  })

  const fetchImpl = (async () =>
    new Response(upstreamBody, {
      status: 200,
      headers: { "content-type": "text/event-stream" },
    })) as typeof fetch

  const handler = createRelayHandler({ upstreamBase: UPSTREAM, fetchImpl })
  const res = await handler(
    new Request(`${UPSTREAM}/backend-api/codex/responses`, { method: "POST", body: "{}" }),
  )
  assert(res.body, "response has no body stream")
  const reader = res.body!.getReader()

  const first = await reader.read()
  assertEquals(first.done, false)
  assertEquals(new TextDecoder().decode(first.value), "A")

  // The second chunk must not be readable yet — the upstream stream is
  // still open at this point, which is exactly what "no buffering" means.
  let secondSettled = false
  const secondRead = reader.read().then((r) => {
    secondSettled = true
    return r
  })
  await new Promise((r) => setTimeout(r, 20))
  assertEquals(secondSettled, false, "second chunk observable before upstream released it")

  releaseB.resolve()
  const second = await secondRead
  assertEquals(second.done, false)
  assertEquals(new TextDecoder().decode(second.value), "B")

  const third = await reader.read()
  assertEquals(third.done, true)
})

// ---------------------------------------------------------------------------
// (c) Path allowlist
// ---------------------------------------------------------------------------

Deno.test("path allowlist: codex and wham pass, everything else 502s with x-relay-fault: path", async () => {
  const fetchImpl = (async () => new Response("ok")) as typeof fetch
  const handler = createRelayHandler({ upstreamBase: UPSTREAM, fetchImpl })

  const codexOk = await handler(
    new Request(`${UPSTREAM}/backend-api/codex/responses`, { method: "POST", body: "{}" }),
  )
  assertEquals(codexOk.status, 200)
  assertEquals(codexOk.headers.get("x-relay-upstream"), "1")

  const whamOk = await handler(new Request(`${UPSTREAM}/backend-api/wham/usage`, { method: "GET" }))
  assertEquals(whamOk.status, 200)
  assertEquals(whamOk.headers.get("x-relay-upstream"), "1")

  const otherBackendApi = await handler(
    new Request(`${UPSTREAM}/backend-api/other`, { method: "GET" }),
  )
  assertEquals(otherBackendApi.status, 502)
  assertEquals(otherBackendApi.headers.get("x-relay-fault"), "path")

  const outsideBackendApi = await handler(new Request(`${UPSTREAM}/api/anything`, { method: "GET" }))
  assertEquals(outsideBackendApi.status, 502)
  assertEquals(outsideBackendApi.headers.get("x-relay-fault"), "path")
})

// ---------------------------------------------------------------------------
// (d) Method allowlist
// ---------------------------------------------------------------------------

Deno.test("method allowlist: DELETE on a valid path 502s with x-relay-fault: method", async () => {
  const fetchImpl = (async () => new Response("should not be reached")) as typeof fetch
  const handler = createRelayHandler({ upstreamBase: UPSTREAM, fetchImpl })

  const res = await handler(
    new Request(`${UPSTREAM}/backend-api/codex/responses`, { method: "DELETE" }),
  )
  assertEquals(res.status, 502)
  assertEquals(res.headers.get("x-relay-fault"), "method")
})

// ---------------------------------------------------------------------------
// (e) Upstream fetch rejects
// ---------------------------------------------------------------------------

Deno.test("upstream fetch throwing becomes a 502 upstream_unreachable fault with a parseable JSON body", async () => {
  const fetchImpl = (async () => {
    throw new TypeError("network down")
  }) as typeof fetch
  const handler = createRelayHandler({ upstreamBase: UPSTREAM, fetchImpl })

  const res = await handler(
    new Request(`${UPSTREAM}/backend-api/codex/responses`, { method: "POST", body: "{}" }),
  )
  assertEquals(res.status, 502)
  assertEquals(res.headers.get("x-relay-fault"), "upstream_unreachable")
  const json = await res.json()
  assertEquals(json, { error: { type: "relay_fault", reason: "upstream_unreachable" } })
})

// ---------------------------------------------------------------------------
// (f) Query preserved
// ---------------------------------------------------------------------------

Deno.test("preserves the query string verbatim in the mapped upstream URL", async () => {
  let capturedUrl: string | undefined
  const fetchImpl = (async (url) => {
    capturedUrl = String(url)
    return new Response("ok")
  }) as typeof fetch
  const handler = createRelayHandler({ fetchImpl }) // default upstreamBase: https://chatgpt.com

  await handler(
    new Request("https://relay.example/backend-api/codex/models?client_version=1.2.3", {
      method: "GET",
    }),
  )
  assertEquals(capturedUrl, "https://chatgpt.com/backend-api/codex/models?client_version=1.2.3")
})

// ---------------------------------------------------------------------------
// (g) Response hygiene
// ---------------------------------------------------------------------------

Deno.test("reduces response headers to content-type + x-relay-upstream, dropping set-cookie and friends", async () => {
  const fetchImpl = (async () =>
    new Response("nope", {
      status: 401,
      headers: {
        "set-cookie": "sess=abc; HttpOnly",
        "x-request-id": "req_123",
        "content-type": "application/json",
      },
    })) as typeof fetch
  const handler = createRelayHandler({ upstreamBase: UPSTREAM, fetchImpl })

  const res = await handler(
    new Request(`${UPSTREAM}/backend-api/codex/responses`, { method: "POST", body: "{}" }),
  )
  assertEquals(res.status, 401)
  assertEquals(res.headers.get("content-type"), "application/json")
  assertEquals(res.headers.get("x-relay-upstream"), "1")
  assert(!res.headers.has("set-cookie"), "set-cookie leaked through")
  assert(!res.headers.has("x-request-id"), "x-request-id leaked through")
})

// ---------------------------------------------------------------------------
// (h) healthz
// ---------------------------------------------------------------------------

Deno.test("GET /healthz returns 200 ok without touching the network or the upstream marker", async () => {
  let fetchCalled = false
  const fetchImpl = (async () => {
    fetchCalled = true
    return new Response("should not be called")
  }) as typeof fetch
  const handler = createRelayHandler({ upstreamBase: UPSTREAM, fetchImpl })

  const res = await handler(new Request(`${UPSTREAM}/healthz`, { method: "GET" }))
  assertEquals(res.status, 200)
  assertEquals(await res.text(), "ok")
  assert(!res.headers.has("x-relay-upstream"), "healthz should not carry the upstream marker")
  assert(!fetchCalled, "healthz must not hit the network")
})

// ---------------------------------------------------------------------------
// (i) POST body forwarded intact
// ---------------------------------------------------------------------------

Deno.test("forwards the POST body bytes intact", async () => {
  const payload = JSON.stringify({ model: "gpt-5.2", input: [{ role: "user", content: "hi 汉字" }] })
  let capturedBody: ArrayBuffer | undefined
  const fetchImpl = (async (_url, init) => {
    capturedBody = init?.body as ArrayBuffer
    return new Response("ok")
  }) as typeof fetch
  const handler = createRelayHandler({ upstreamBase: UPSTREAM, fetchImpl })

  await handler(
    new Request(`${UPSTREAM}/backend-api/codex/responses`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: payload,
    }),
  )

  assert(capturedBody instanceof ArrayBuffer, "body was not forwarded as raw bytes")
  assertEquals(new TextDecoder().decode(capturedBody), payload)
})
