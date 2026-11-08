import { afterEach, expect, it, vi } from "vitest"
import { createSSRApp, h, type Component } from "vue"
import { renderToString } from "vue/server-renderer"
import CustomProviderCard from "@/components/providers/CustomProviderCard.vue"
import RequestEditSwitches from "@/components/providers/RequestEditSwitches.vue"
import { updateProviderSettings } from "@/services/api"
import type { CustomProvider } from "@/types"

// Server-rendered like providerRows.test.ts: the app has no DOM test
// environment, and what these pin is which switches and tags a row shows.

async function render(component: Component, props: Record<string, unknown>) {
  return renderToString(createSSRApp({ render: () => h(component, props) }))
}

function endpoint(overrides: Partial<CustomProvider> = {}): CustomProvider {
  return {
    id: "cp-1",
    slug: "gateway",
    name: "Gateway",
    format: "anthropic",
    base_url: "https://gateway.example.com",
    count_tokens_url: null,
    models_mode: "auto",
    manual_models: [],
    key_mask: "sk-abc…f3a2",
    account_id: "acct-1",
    status: "active",
    sort_order: 0,
    created_at: "2026-11-08T00:00:00Z",
    updated_at: "2026-11-08T00:00:00Z",
    ...overrides,
  }
}

afterEach(() => {
  vi.unstubAllGlobals()
})

it("offers fast mode only where the provider has a line for it", async () => {
  const both = await render(RequestEditSwitches, {
    stripFastMode: true,
    stripClaudeCodeLine: false,
    fastModeHint: "Requests go at standard speed.",
    lineHint: "Drops the line.",
  })
  expect(both).toContain("Remove fast mode")
  expect(both).toContain("Remove the Claude Code line")
  // Each box paints the stored value, not a default.
  expect(both).toMatch(/<input[^>]*type="checkbox"[^>]*checked[^>]*>[\s\S]*Remove fast mode/)
  expect(both.match(/checked/g)).toHaveLength(1)

  // Grok and Antigravity never forward fast mode: no hint, no switch.
  const lineOnly = await render(RequestEditSwitches, {
    stripFastMode: false,
    stripClaudeCodeLine: true,
    lineHint: "Drops the line.",
  })
  expect(lineOnly).not.toContain("Remove fast mode")
  expect(lineOnly).toContain("Remove the Claude Code line")
})

it("tags an endpoint whose switch is on while the gate is closed, and keeps the switches behind it", async () => {
  const closed = await render(CustomProviderCard, {
    provider: endpoint({ strip_fast_mode: true }),
  })
  expect(closed).toContain("Fast mode removed")
  expect(closed).not.toContain("Claude Code line removed")
  expect(closed).not.toContain("Remove fast mode")

  const open = await render(CustomProviderCard, {
    provider: endpoint({ strip_claude_code_line: true }),
    editing: true,
  })
  expect(open).toContain("Claude Code line removed")
  expect(open).toContain("Requests to this endpoint go at standard speed.")
  expect(open).toContain("sent to this endpoint.")
})

it("reads an endpoint cached before the switches existed as both off", async () => {
  const html = await render(CustomProviderCard, { provider: endpoint(), editing: true })
  expect(html).not.toContain("Fast mode removed")
  expect(html).not.toContain("checked")
})

it("sends only the setting that changed, to the provider's own row", async () => {
  const fetch = vi.fn(async () =>
    new Response(
      JSON.stringify({ ok: true, strategy: "ordered", strip_fast_mode: true, strip_claude_code_line: false }),
      { status: 200 },
    ),
  )
  vi.stubGlobal("fetch", fetch)

  const stored = await updateProviderSettings("my gateway", { strip_fast_mode: true })

  const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit]
  expect(url).toBe("/api/providers/my%20gateway")
  expect(init.method).toBe("PATCH")
  // A switch write that also carried the strategy (or the other switch) would
  // overwrite a value the user did not touch.
  expect(JSON.parse(init.body as string)).toEqual({ strip_fast_mode: true })
  expect(stored).toEqual({ strategy: "ordered", strip_fast_mode: true, strip_claude_code_line: false })
})
