import { afterEach, describe, expect, it } from "vitest"
import {
  CODEX_CLIENT_VERSION,
  CODEX_MODEL_MIRROR_URLS,
  CODEX_MODELS_ENDPOINT,
  CODEX_USER_AGENT,
  fetchCodexModels,
} from "../src/providers/codex_models"

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  })
}

describe("fetchCodexModels", () => {
  it("fetches the live catalog with the Codex CLI URL and headers", async () => {
    const requests: Array<{ url: string; init?: RequestInit }> = []
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      requests.push({ url, init })
      return jsonResponse({
        models: [
          { slug: "gpt-5.6-sol", display_name: "GPT-5.6-Sol", visibility: "list" },
          { slug: "gpt-5.5", display_name: null, visibility: "list" },
        ],
      })
    }) as typeof fetch

    const result = await fetchCodexModels("access-token", "account-123")

    expect(result).toEqual({
      models: [
        { id: "gpt-5.6-sol", display_name: "GPT-5.6-Sol" },
        { id: "gpt-5.5", display_name: null },
      ],
      error: null,
    })
    expect(requests).toHaveLength(1)
    expect(requests[0]?.url).toBe(
      `${CODEX_MODELS_ENDPOINT}?client_version=${CODEX_CLIENT_VERSION}`,
    )
    expect(requests[0]?.init?.method).toBe("GET")
    const headers = new Headers(requests[0]?.init?.headers)
    expect(headers.get("accept")).toBe("application/json")
    expect(headers.get("authorization")).toBe("Bearer access-token")
    expect(headers.get("originator")).toBe("codex_cli_rs")
    expect(headers.get("user-agent")).toBe(CODEX_USER_AGENT)
    expect(headers.get("chatgpt-account-id")).toBe("account-123")
  })

  it("omits Chatgpt-Account-Id when accountId is empty", async () => {
    let capturedInit: RequestInit | undefined
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      capturedInit = init
      return jsonResponse({ models: [] })
    }) as typeof fetch

    await fetchCodexModels("access-token", "")

    const headers = new Headers(capturedInit?.headers)
    expect(headers.has("chatgpt-account-id")).toBe(false)
  })

  it("filters hidden models, skips invalid slugs, and deduplicates slugs", async () => {
    globalThis.fetch = (async () =>
      jsonResponse({
        models: [
          { slug: "hidden", display_name: "Hidden", visibility: "hide" },
          { display_name: "Missing slug" },
          { slug: "", display_name: "Empty slug" },
          { slug: 123, display_name: "Non-string slug" },
          { slug: "gpt-5.5", display_name: "First" },
          { slug: "gpt-5.5", display_name: "Second" },
          { slug: "visible", display_name: "Visible" },
        ],
      })
    ) as typeof fetch

    const result = await fetchCodexModels("access-token", "account-123")

    expect(result).toEqual({
      models: [
        { id: "gpt-5.5", display_name: "First" },
        { id: "visible", display_name: "Visible" },
      ],
      error: null,
    })
  })

  it("falls through a 403 HTML bot wall to mirror 1 without auth headers", async () => {
    const requests: Array<{ url: string; init?: RequestInit }> = []
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      requests.push({ url, init })
      if (requests.length === 1) {
        return new Response("<html>Just a moment...</html>", {
          status: 403,
          headers: { "content-type": "text/html" },
        })
      }
      return jsonResponse({ models: [{ slug: "mirror-model", display_name: "Mirror" }] })
    }) as typeof fetch

    const result = await fetchCodexModels("access-token", "account-123")

    expect(result).toEqual({
      models: [{ id: "mirror-model", display_name: "Mirror" }],
      error: null,
    })
    expect(requests.map((request) => request.url)).toEqual([
      `${CODEX_MODELS_ENDPOINT}?client_version=${CODEX_CLIENT_VERSION}`,
      CODEX_MODEL_MIRROR_URLS[0],
    ])
    const mirrorHeaders = new Headers(requests[1]?.init?.headers)
    expect(mirrorHeaders.has("authorization")).toBe(false)
    expect(mirrorHeaders.has("chatgpt-account-id")).toBe(false)
  })

  it("uses mirror 2 when mirror 1 fails", async () => {
    const urls: string[] = []
    globalThis.fetch = (async (url: string) => {
      urls.push(url)
      if (urls.length === 1) return new Response("blocked", { status: 403 })
      if (urls.length === 2) return new Response("unavailable", { status: 503 })
      return jsonResponse({ models: [{ slug: "mirror-2" }] })
    }) as typeof fetch

    const result = await fetchCodexModels("access-token", "account-123")

    expect(result).toEqual({
      models: [{ id: "mirror-2", display_name: null }],
      error: null,
    })
    expect(urls).toEqual([
      `${CODEX_MODELS_ENDPOINT}?client_version=${CODEX_CLIENT_VERSION}`,
      CODEX_MODEL_MIRROR_URLS[0],
      CODEX_MODEL_MIRROR_URLS[1],
    ])
  })

  it("returns an error instead of throwing when every source fails", async () => {
    let calls = 0
    globalThis.fetch = (async () => {
      calls += 1
      if (calls === 1) throw new Error("network down")
      if (calls === 2) return new Response("not json", { status: 502 })
      return new Response("still not json", { status: 200 })
    }) as typeof fetch

    const result = await fetchCodexModels("access-token", "account-123")

    expect(calls).toBe(3)
    expect(result.models).toEqual([])
    expect(result.error).toEqual("models response is not JSON")
  })

  it("falls through malformed JSON without throwing", async () => {
    const urls: string[] = []
    globalThis.fetch = (async (url: string) => {
      urls.push(url)
      if (urls.length === 1) {
        return new Response("{\"models\":", {
          status: 200,
          headers: { "content-type": "application/json" },
        })
      }
      return jsonResponse({ models: [{ slug: "after-malformed" }] })
    }) as typeof fetch

    await expect(fetchCodexModels("access-token", "account-123")).resolves.toEqual({
      models: [{ id: "after-malformed", display_name: null }],
      error: null,
    })
    expect(urls).toEqual([
      `${CODEX_MODELS_ENDPOINT}?client_version=${CODEX_CLIENT_VERSION}`,
      CODEX_MODEL_MIRROR_URLS[0],
    ])
  })

  it("falls through a response without a models array", async () => {
    globalThis.fetch = (async () => jsonResponse({ models: {} })) as typeof fetch

    const result = await fetchCodexModels("access-token", "account-123")

    expect(result.models).toEqual([])
    expect(result.error).not.toBeNull()
  })
})
