import { afterEach, expect, it, vi } from "vitest"
import { createSSRApp, h, type Component } from "vue"
import { renderToString } from "vue/server-renderer"
import AccountCard from "@/components/providers/AccountCard.vue"
import CustomProviderCard from "@/components/providers/CustomProviderCard.vue"
import RequestEditSwitches from "@/components/providers/RequestEditSwitches.vue"
import { updateAccountEdits } from "@/services/api"
import type { CustomProvider, ProviderAccount } from "@/types"

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
    strip_fast_mode: false,
    strip_claude_code_line: false,
    force_cache_ttl_1h: false,
    ...overrides,
  }
}

function account(overrides: Partial<ProviderAccount> = {}): ProviderAccount {
  return {
    id: "acct-1",
    status: "active",
    label: "owner@example.com",
    custom_label: null,
    account: { email: "owner@example.com", plan_type: "claude_max" },
    usage: { windows: [{ label: "5h", utilization: 40, resets_at: null }] },
    error: null,
    allow_credits: false,
    on_credits: false,
    strip_fast_mode: false,
    strip_claude_code_line: false,
    force_cache_ttl_1h: false,
    benched: false,
    sign_in_expires_at: null,
    ...overrides,
  }
}

const shared = { teamId: "team-1", teamName: "Team", ownerLabel: "owner@example.com" }

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
  expect(both).toContain("Hide Claude Code identity")
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
  expect(lineOnly).toContain("Hide Claude Code identity")
})

it("tags an endpoint whose switch is on while the gate is closed, and keeps the switches behind it", async () => {
  const closed = await render(CustomProviderCard, {
    provider: endpoint({ strip_fast_mode: true }),
  })
  expect(closed).toContain("Fast mode removed")
  expect(closed).not.toContain("Claude Code identity hidden")
  expect(closed).not.toContain("Remove fast mode")

  const open = await render(CustomProviderCard, {
    provider: endpoint({ strip_claude_code_line: true }),
    editing: true,
  })
  expect(open).toContain("Claude Code identity hidden")
  expect(open).toContain("Requests to this endpoint go at standard speed.")
  expect(open).toContain("sent to this endpoint.")
})

it("gives each own account its own block, behind the gate, with that account's values", async () => {
  const first = await render(AccountCard, {
    account: account({ strip_fast_mode: true }),
    provider: "claude-code",
    editing: true,
  })
  const second = await render(AccountCard, {
    account: account({ id: "acct-2" }),
    provider: "claude-code",
    editing: true,
  })
  expect(first).toContain("Request edits for this account")
  expect(first).toContain("Claude bills fast mode to usage credits.")
  expect(first).toContain("Fast mode removed")
  expect(first.match(/checked/g)).toHaveLength(1)
  // The second row of the same pool keeps its own value.
  expect(second).toContain("Request edits for this account")
  expect(second).not.toContain("Fast mode removed")
  expect(second).not.toContain("checked")

  const closed = await render(AccountCard, {
    account: account({ strip_fast_mode: true }),
    provider: "claude-code",
  })
  expect(closed).toContain("Fast mode removed")
  expect(closed).not.toContain("Request edits for this account")
})

it("offers the cache switch only on Claude Code rows and Anthropic-format endpoints", async () => {
  const claude = await render(AccountCard, {
    account: account({ force_cache_ttl_1h: true }),
    provider: "claude-code",
    editing: true,
  })
  expect(claude).toContain("Keep prompt cache for 1 hour")
  expect(claude).toContain("sent to this account lasts an hour instead of five minutes.")
  expect(claude).toContain("Cache kept 1 hour")
  expect(claude.match(/checked/g)).toHaveLength(1)
  // Their conversion drops cache markers, so the switch would change nothing.
  for (const provider of ["codex", "grok", "antigravity"]) {
    const html = await render(AccountCard, { account: account(), provider, editing: true })
    expect(html).not.toContain("Keep prompt cache for 1 hour")
  }

  const anthropic = await render(CustomProviderCard, { provider: endpoint(), editing: true })
  expect(anthropic).toContain("in requests to this endpoint lasts an hour")
  const openai = await render(CustomProviderCard, { provider: endpoint({ format: "openai" }), editing: true })
  expect(openai).toContain("Hide Claude Code identity")
  expect(openai).not.toContain("Keep prompt cache for 1 hour")

  // The tag shows with the gate closed, and on a shared row as the owner's setting.
  const closed = await render(CustomProviderCard, { provider: endpoint({ force_cache_ttl_1h: true }) })
  expect(closed).toContain("Cache kept 1 hour")
  expect(closed).not.toContain("Keep prompt cache for 1 hour")
  const lent = await render(AccountCard, {
    account: account({ share: shared, force_cache_ttl_1h: true }),
    provider: "claude-code",
    editing: true,
  })
  expect(lent).toContain("Cache kept 1 hour")
  expect(lent).not.toContain("Keep prompt cache for 1 hour")
})

it("offers only the line on grok and antigravity rows", async () => {
  for (const provider of ["grok", "antigravity"]) {
    const html = await render(AccountCard, { account: account(), provider, editing: true })
    expect(html).toContain("Hide Claude Code identity")
    expect(html).not.toContain("Remove fast mode")
  }
})

it("shows a shared row the owner's switches as tags and gives it no block", async () => {
  const html = await render(AccountCard, {
    account: account({ share: shared, strip_claude_code_line: true }),
    provider: "claude-code",
    editing: true,
  })
  expect(html).toContain("Claude Code identity hidden")
  expect(html).not.toContain("Request edits for this account")
})

it("sends only the switch that changed, to that account's own row", async () => {
  const fetch = vi.fn(async () =>
    new Response(
      JSON.stringify({
        ok: true,
        custom_label: null,
        allow_credits: false,
        strip_fast_mode: true,
        strip_claude_code_line: false,
        force_cache_ttl_1h: false,
      }),
      { status: 200 },
    ),
  )
  vi.stubGlobal("fetch", fetch)

  const stored = await updateAccountEdits("my gateway", "acct/1", { strip_fast_mode: true })

  const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit]
  expect(url).toBe("/api/providers/my%20gateway/accounts/acct%2F1")
  expect(init.method).toBe("PATCH")
  // A write that also carried the other switch would overwrite a value the
  // user did not touch.
  expect(JSON.parse(init.body as string)).toEqual({ strip_fast_mode: true })
  expect(stored).toEqual({ strip_fast_mode: true, strip_claude_code_line: false, force_cache_ttl_1h: false })
})
