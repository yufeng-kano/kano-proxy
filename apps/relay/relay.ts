/**
 * Codex egress relay — a stateless byte pipe to https://chatgpt.com.
 *
 * Design and wire contract: docs/codex-relay.md (repo root). This file holds
 * no credentials at rest, keeps no state, and logs nothing itself — Cloud
 * Run's own platform request logs (URL, status, latency) are the only
 * record of what passed through, per docs/codex-relay.md "Operations".
 *
 * The relay is not a general reverse proxy: it serves exactly one upstream,
 * hardcoded below, and only two path prefixes. Even a stolen invocation path
 * (a leaked Cloud Run IAM token) can only reach the codex endpoints.
 */

const DEFAULT_UPSTREAM_BASE = "https://chatgpt.com"

/**
 * Inbound headers rebuilt into the upstream request — nothing else crosses.
 * This is the load-bearing line of the whole design: the inbound request at
 * Cloud Run already carries `CF-Worker` / `CF-Connecting-IP` (Cloudflare
 * stamps them on the Worker's own outbound fetch *to* this relay), and any
 * generic reverse-proxy behavior that forwards headers verbatim would
 * reproduce the exact 403 this relay exists to escape. Only names on this
 * list are ever read from the inbound request.
 */
const ALLOWED_REQUEST_HEADERS = [
  "authorization",
  "chatgpt-account-id",
  "session_id",
  "originator",
  "user-agent",
  "content-type",
  "accept",
  "accept-language",
  "openai-beta",
] as const

const ALLOWED_METHODS = new Set(["GET", "POST"])
const ALLOWED_PATH_PREFIXES = ["/backend-api/codex/", "/backend-api/wham/"] as const

export type RelayFaultReason = "path" | "method" | "upstream_unreachable"

export type CreateRelayHandlerOptions = {
  upstreamBase?: string
  fetchImpl?: typeof fetch
}

export function createRelayHandler(
  opts: CreateRelayHandlerOptions = {},
): (req: Request) => Promise<Response> {
  const upstreamBase = opts.upstreamBase ?? DEFAULT_UPSTREAM_BASE
  const doFetch = opts.fetchImpl ?? fetch

  return async function handleRequest(req: Request): Promise<Response> {
    const url = new URL(req.url)

    if (req.method === "GET" && url.pathname === "/healthz") {
      return new Response("ok", { status: 200 })
    }

    // Local compute, never proxied (docs/codex-relay.md § Token counting).
    // Handled before the path allowlist: this is the one endpoint that is
    // not a byte pipe to chatgpt.com.
    if (req.method === "POST" && url.pathname === "/count-tokens") {
      return handleCountTokens(req)
    }

    // Method and path are self-errors, not upstream ones — reject before
    // ever touching the network. Method is checked first: a wrong-method
    // request to an otherwise-valid path is a "method" fault, not "path".
    if (!ALLOWED_METHODS.has(req.method)) {
      return relayFault("method")
    }
    if (!ALLOWED_PATH_PREFIXES.some((prefix) => url.pathname.startsWith(prefix))) {
      return relayFault("path")
    }

    const headers = new Headers()
    for (const name of ALLOWED_REQUEST_HEADERS) {
      const value = req.headers.get(name)
      if (value !== null) headers.set(name, value)
    }
    // Bytes cross the wire 1:1 — no decompress/reframe bookkeeping needed on
    // either side (bandwidth is not the constraint here; see docs).
    headers.set("accept-encoding", "identity")

    // Deliberate asymmetry from the response path below: request bodies are
    // bounded JSON, and reading them fully gives a known Content-Length,
    // avoiding chunked-POST ambiguity at OpenAI's edge. GET sends no body.
    const body = req.method === "POST" ? await req.arrayBuffer() : undefined

    const upstreamUrl = `${upstreamBase}${url.pathname}${url.search}`

    let upstream: Response
    try {
      upstream = await doFetch(upstreamUrl, {
        method: req.method,
        headers,
        body,
        redirect: "follow",
        // Client disconnect (an agent stopped mid-run) aborts the upstream
        // fetch, so the upstream stops generating and Cloud Run stops
        // billing the wait.
        signal: req.signal,
      })
    } catch {
      return relayFault("upstream_unreachable")
    }

    const outHeaders = new Headers()
    const contentType = upstream.headers.get("content-type")
    if (contentType) outHeaders.set("content-type", contentType)
    outHeaders.set("x-relay-upstream", "1")

    // Straight pipe: the upstream body streams through untouched — never
    // `await text()`/`arrayBuffer()`, no Content-Length set by hand. This is
    // the streaming rule in CLAUDE.md, pinned by relay_test.ts rather than
    // by care.
    return new Response(upstream.body, {
      status: upstream.status,
      headers: outHeaders,
    })
  }
}

/**
 * `POST /count-tokens` — `{texts: string[]}` in, `{tokens: <o200k_base
 * total>}` out. The Worker owns the Anthropic-body → text serialization;
 * this side only tokenizes. A malformed body is the caller's 400, not an
 * `x-relay-fault` (the Worker degrades any non-200 to its sentinel zero).
 * Bodies are prompt content — never logged, same as proxied traffic.
 */
async function handleCountTokens(req: Request): Promise<Response> {
  let texts: unknown
  try {
    texts = ((await req.json()) as { texts?: unknown }).texts
  } catch {
    texts = undefined
  }
  if (!Array.isArray(texts) || !texts.every((t) => typeof t === "string")) {
    return Response.json({ error: { type: "count_bad_request" } }, { status: 400 })
  }
  const { countTokens } = await import("./tokenizer.ts")
  return Response.json(
    { tokens: countTokens(texts as string[]) },
    { headers: { "x-relay-count": "1" } },
  )
}

/**
 * The relay itself failed (never the upstream) — always 502, never
 * 401/402/403/429. Those four bench pool accounts in the Worker
 * (`pool/acquire.ts` `FAILOVER_STATUS`); a relay misconfiguration must
 * degrade the codex route, not poison codex account state.
 */
function relayFault(reason: RelayFaultReason): Response {
  return Response.json(
    { error: { type: "relay_fault", reason } },
    { status: 502, headers: { "x-relay-fault": reason } },
  )
}
