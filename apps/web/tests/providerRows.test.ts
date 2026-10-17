import { expect, it } from "vitest"
import { createSSRApp, h, type Component } from "vue"
import { renderToString, type SSRContext } from "vue/server-renderer"
import AccountCard from "@/components/providers/AccountCard.vue"
import AddAccountDialog from "@/components/AddAccountDialog.vue"
import type { ProviderAccount } from "@/types"

// Rendered on the server renderer: the app has no DOM test environment, and
// what these tests pin is what a resting row and the dialog's first step show.

async function render(component: Component, props: Record<string, unknown>) {
  const ctx: SSRContext = {}
  const html = await renderToString(createSSRApp({ render: () => h(component, props) }), ctx)
  // Modal teleports to <body>; its markup lands in the context, not the string.
  return html + Object.values(ctx.teleports ?? {}).join("")
}

function account(overrides: Partial<ProviderAccount> = {}): ProviderAccount {
  return {
    id: "acct-1",
    priority: 0,
    status: "active",
    label: "owner@example.com",
    custom_label: null,
    account: { email: "owner@example.com", plan_type: "claude_max" },
    usage: { windows: [{ label: "5h", utilization: 40, resets_at: null }] },
    error: null,
    stale: false,
    allow_credits: false,
    on_credits: false,
    ...overrides,
  }
}

function codexAccount(credits: unknown, overrides: Partial<ProviderAccount> = {}) {
  return account({
    account: { email: "owner@example.com", plan_type: "plus", credits },
    usage: { windows: [{ label: "Week", utilization: 100, resets_at: null }] },
    ...overrides,
  })
}

const expired = account({ status: "reauth", error: "usage 401", stale: true })

it("offers Sign in again on an expired row while the section is not being edited", async () => {
  const html = await render(AccountCard, { account: expired, provider: "claude-code" })
  expect(html).toContain("Sign-in expired")
  expect(html).toContain('aria-label="Sign in again as owner@example.com"')
  // The gate is closed, so the row's other actions are still absent.
  expect(html).not.toContain("Remove owner@example.com")
})

it("never prints the raw auth error or promises usage on an expired row", async () => {
  const withBars = await render(AccountCard, { account: expired, provider: "claude-code" })
  expect(withBars).not.toContain("usage 401")
  expect(withBars).toContain("frozen")

  const noBars = await render(AccountCard, {
    account: { ...expired, usage: null },
    provider: "claude-code",
  })
  expect(noBars).not.toContain("Usage will appear")
})

it("leaves Sign in again to the owner on a borrowed row", async () => {
  const html = await render(AccountCard, {
    account: { ...expired, share: { teamId: "t", teamName: "Team", ownerLabel: "Owner" } },
    provider: "claude-code",
  })
  expect(html).toContain("Sign-in expired")
  expect(html).not.toContain("Sign in again")
})

it("states another probe failure inline, keeping the upstream text for hover and screen readers", async () => {
  const html = await render(AccountCard, {
    account: account({ error: "usage 500 internal" }),
    provider: "claude-code",
  })
  expect(html).not.toContain('role="alert"')
  expect(html).toContain('title="usage 500 internal"')
  expect(html).toContain("Couldn&#39;t read usage: usage 500 internal")
  expect(html).not.toContain("Sign in again")
})

it("keeps the probe errors the operator cannot act on off the row", async () => {
  for (const error of ["usage 429", "edge blocked"]) {
    const html = await render(AccountCard, { account: account({ error }), provider: "codex" })
    expect(html).not.toContain(error)
    expect(html).not.toContain("Couldn&#39;t read usage")
  }
})

it("signs in again without offering the manual-token step, which would add a new row", async () => {
  const reconnect = await render(AddAccountDialog, {
    provider: "claude-code",
    providerName: "Claude Code",
    reconnect: { id: "acct-1", name: "owner@example.com" },
  })
  expect(reconnect).toContain("Sign in again as owner@example.com")
  expect(reconnect).not.toContain("Enter tokens manually")

  const add = await render(AddAccountDialog, { provider: "claude-code", providerName: "Claude Code" })
  expect(add).toContain("Enter tokens manually")
})

it("prints a Codex credit balance only when upstream reported one", async () => {
  const balance = await render(AccountCard, {
    account: codexAccount({ has_credits: true, unlimited: false, balance: "1234.5" }),
    provider: "codex",
  })
  expect(balance).toContain("1,234.5 credits left")

  // Not a number: upstream's text is shown as is, never coerced to NaN or 0.
  const raw = await render(AccountCard, {
    account: codexAccount({ has_credits: true, unlimited: false, balance: "n/a" }),
    provider: "codex",
  })
  expect(raw).toContain("n/a credits left")

  const unlimited = await render(AccountCard, {
    account: codexAccount({ has_credits: true, unlimited: true, balance: null }),
    provider: "codex",
  })
  expect(unlimited).toContain("Unlimited credits")

  for (const credits of [null, undefined, { has_credits: false, unlimited: false, balance: "0" }]) {
    const html = await render(AccountCard, { account: codexAccount(credits), provider: "codex" })
    expect(html).not.toContain("credits left")
    expect(html).not.toContain("Unlimited credits")
  }
})

it("offers the credits opt-in only to the owner of a Codex row with credits, behind the gate", async () => {
  const withCredits = codexAccount({ has_credits: true, unlimited: false, balance: "41.5" })
  const label = "Use credits after limits"

  const open = await render(AccountCard, { account: withCredits, provider: "codex", editing: true })
  expect(open).toContain(label)
  expect(open).not.toMatch(/type="checkbox"[^>]*checked/)

  const optedIn = await render(AccountCard, {
    account: { ...withCredits, allow_credits: true },
    provider: "codex",
    editing: true,
  })
  expect(optedIn).toMatch(/type="checkbox"[^>]*checked/)

  const resting = await render(AccountCard, { account: withCredits, provider: "codex" })
  expect(resting).not.toContain(label)

  const shared = await render(AccountCard, {
    account: { ...withCredits, share: { teamId: "t", teamName: "Team", ownerLabel: "Owner" } },
    provider: "codex",
    editing: true,
  })
  expect(shared).not.toContain(label)

  const noCredits = await render(AccountCard, {
    account: codexAccount({ has_credits: false, unlimited: false, balance: "0" }),
    provider: "codex",
    editing: true,
  })
  expect(noCredits).not.toContain(label)

  const otherProvider = await render(AccountCard, {
    account: withCredits,
    provider: "claude-code",
    editing: true,
  })
  expect(otherProvider).not.toContain(label)
})

it("keeps the opt-in on an opted-in row whose credits ran out, so it can be turned off", async () => {
  const html = await render(AccountCard, {
    account: codexAccount(
      { has_credits: false, unlimited: false, balance: "0" },
      { allow_credits: true },
    ),
    provider: "codex",
    editing: true,
  })
  expect(html).toContain("Use credits after limits")
})

it("marks a row that routes only on credits, without opening the gate", async () => {
  const credits = { has_credits: true, unlimited: false, balance: "41.5" }
  const on = await render(AccountCard, {
    account: codexAccount(credits, { allow_credits: true, on_credits: true }),
    provider: "codex",
  })
  expect(on).toContain("On credits")
  expect(on).toContain("Active")
  expect(on).not.toContain("Limit reached")

  const off = await render(AccountCard, { account: codexAccount(credits), provider: "codex" })
  expect(off).not.toContain("On credits")
})
