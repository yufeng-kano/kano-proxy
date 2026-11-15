import { expect, it } from "vitest"
import { createSSRApp, h } from "vue"
import { renderToString } from "vue/server-renderer"
import SharedEndpointCard from "@/components/providers/SharedEndpointCard.vue"
import { customEndpointCount, reachableShared } from "@/composables/useCustomProviders"
import type { CustomProvider, SharedCustomProvider } from "@/types"

// Rendered on the server renderer, like the account rows: the app has no DOM
// test environment, and what is pinned here is what a resting row shows.

function shared(overrides: Partial<SharedCustomProvider> = {}): SharedCustomProvider {
  return {
    id: "cprov_team",
    slug: "team-gw",
    name: "Team gateway",
    format: "anthropic",
    status: "active",
    account_id: "acc_owner",
    share: { teamId: "t", teamName: "Team", ownerLabel: "owner@example.com" },
    usage: { windows: [{ label: "Allowance", utilization: 40, resets_at: null }] },
    shadowed: false,
    strip_fast_mode: true,
    strip_claude_code_line: false,
    ...overrides,
  }
}

function render(props: Record<string, unknown>) {
  return renderToString(createSSRApp({ render: () => h(SharedEndpointCard, props) }))
}

it("shows a borrowed endpoint's prefix, lender, owner's edits and bars, with no controls", async () => {
  // `editing` is what the section passes its own rows; a shared row takes no gate at all.
  const html = await render({ provider: shared(), editing: true })
  expect(html).toContain("Team gateway")
  expect(html).toContain("Anthropic")
  expect(html).toContain("team-gw/*")
  expect(html).toContain("Shared by owner@example.com")
  expect(html).toContain("Fast mode removed")
  expect(html).not.toContain("Claude Code identity hidden")
  expect(html).toContain("Allowance")
  expect(html).toContain("Active")
  expect(html).not.toContain("<button")
  expect(html).not.toContain("Unavailable")
})

it("reads Unavailable with its one reason when the viewer's own prefix shadows it", async () => {
  const html = await render({ provider: shared({ shadowed: true, usage: null }) })
  expect(html).toContain("Unavailable")
  expect(html).toContain("You already have a provider with this prefix")
  expect(html).not.toContain(">Active<")
  expect(html).not.toContain('class="windows"')
})

it("counts reachable shared endpoints on the Custom tab, and nothing before the list loads", () => {
  const own = [{ id: "cprov_own" } as CustomProvider]
  expect(customEndpointCount(null, [shared()])).toBeNull()
  expect(customEndpointCount(own, null)).toBe(1)
  expect(customEndpointCount(own, [shared(), shared({ id: "cprov_dup", shadowed: true })])).toBe(2)
  expect(customEndpointCount([], [])).toBe(0)
})

it("offers only unshadowed shared endpoints to the pages that route, filter or list by slug", () => {
  const reachable = shared()
  const mine = shared({ id: "cprov_dup", slug: "mine", shadowed: true })
  expect(reachableShared([reachable, mine])).toEqual([reachable])
  expect(reachableShared(null)).toEqual([])
})
