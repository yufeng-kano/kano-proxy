import { expect, it } from "vitest"
import { createSSRApp, defineComponent, h, type Component } from "vue"
import { renderToString, type SSRContext } from "vue/server-renderer"
import AccountCard from "@/components/providers/AccountCard.vue"
import AddAccountDialog from "@/components/AddAccountDialog.vue"
import CustomProviderCard from "@/components/providers/CustomProviderCard.vue"
import { webExtensionsKey, type WebExtensions } from "@/extensions"
import type { CustomProvider, ProviderAccount } from "@/types"

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

function codexAccount(credits: unknown, overrides: Partial<ProviderAccount> = {}) {
  return account({
    account: { email: "owner@example.com", plan_type: "plus", credits },
    usage: { windows: [{ label: "Week", utilization: 100, resets_at: null }] },
    ...overrides,
  })
}

const expired = account({ status: "reauth", error: "usage 401" })

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

it("warns on a row whose sign-in is about to stop working, and offers the fix beside it", async () => {
  const soon = new Date(Date.now() + 20 * 60_000).toISOString()
  const expiring = account({ status: "active", sign_in_expires_at: soon })
  const html = await render(AccountCard, { account: expiring, provider: "claude-code" })
  // Still serving, so the dot is not the one that says it.
  expect(html).toContain("Active")
  expect(html).not.toContain("Sign-in expired")
  expect(html).toContain("Sign-in ends in 20 minutes")
  // A stated end days out reads in days, in the same badge.
  const days = new Date(Date.now() + 3 * 86_400_000).toISOString()
  const dated = await render(AccountCard, {
    account: account({ status: "active", sign_in_expires_at: days }),
    provider: "claude-code",
  })
  expect(dated).toContain("Sign-in ends in 3 days")
  expect(html).toContain('aria-label="Sign in again as owner@example.com"')

  // Once it has expired the status says so, and the notice would only repeat it.
  const gone = await render(AccountCard, {
    account: { ...expiring, status: "reauth" },
    provider: "claude-code",
  })
  expect(gone).toContain("Sign-in expired")
  expect(gone).not.toContain("Sign-in ends")

  // A borrower is never told about the owner's sign-in.
  const borrowed = await render(AccountCard, {
    account: { ...expiring, share: { teamId: "t", teamName: "Team", ownerLabel: "Owner" } },
    provider: "claude-code",
  })
  expect(borrowed).not.toContain("Sign-in ends")
  expect(borrowed).not.toContain("Sign in again")
})

it("offers Sign in again on every row of the viewer's own while the section is being edited", async () => {
  const again = 'aria-label="Sign in again as owner@example.com"'
  const working = account({ status: "active" })
  // A working row at rest is identity and usage only.
  expect(await render(AccountCard, { account: working, provider: "claude-code" })).not.toContain(again)
  expect(await render(AccountCard, { account: working, provider: "claude-code", editing: true })).toContain(again)
  // Once, even where the row already offered it at rest.
  const expiredOpen = await render(AccountCard, { account: expired, provider: "claude-code", editing: true })
  expect(expiredOpen.split(again).length - 1).toBe(1)
  // Signing in is the owner's alone, gate or no gate.
  const borrowed = await render(AccountCard, {
    account: { ...working, share: { teamId: "t", teamName: "Team", ownerLabel: "Owner" } },
    provider: "claude-code",
    editing: true,
  })
  expect(borrowed).not.toContain("Sign in again")
})

it("offers Resume wherever a bench is running, an expired sign-in included", async () => {
  const resume = 'aria-label="Resume owner@example.com"'
  const row = (overrides: Partial<ProviderAccount>, editing = true) =>
    render(AccountCard, { account: account(overrides), provider: "claude-code", editing })

  expect(await row({ status: "benched", benched: true })).toContain(resume)
  // The 401 bench is how the proxy paces its retries; clearing it retries now.
  expect(await row({ status: "reauth", benched: true })).toContain(resume)
  // Nothing to clear once the bench has run out, though the sign-in is still expired.
  expect(await row({ status: "reauth", benched: false })).not.toContain(resume)
  expect(await row({ status: "limited" })).not.toContain(resume)
  // Behind the section's gate, like the row's other actions.
  expect(await row({ status: "reauth", benched: true }, false)).not.toContain(resume)
  expect(
    await row({ status: "benched", benched: true, share: { teamId: "t", teamName: "Team", ownerLabel: "Owner" } }),
  ).not.toContain(resume)
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

it("prints a Codex credit balance in whole credits, only when upstream reported one", async () => {
  // Upstream's fraction is noise beside a balance this size; the Codex CLI rounds it too.
  const balance = await render(AccountCard, {
    account: codexAccount({ has_credits: true, unlimited: false, balance: "12345.678" }),
    provider: "codex",
  })
  expect(balance).toContain(">12,346 left<")
  expect(balance).not.toContain("12,345.678")

  // Under one credit the fraction is the whole balance: rounding would print 0
  // on a row that still routes on it.
  const small = await render(AccountCard, {
    account: codexAccount({ has_credits: true, unlimited: false, balance: "0.4" }),
    provider: "codex",
  })
  expect(small).toContain(">0.4 left<")

  // Not a number: upstream's text is shown as is, never coerced to NaN or 0.
  const raw = await render(AccountCard, {
    account: codexAccount({ has_credits: true, unlimited: false, balance: "n/a" }),
    provider: "codex",
  })
  expect(raw).toContain(">n/a left<")

  const unlimited = await render(AccountCard, {
    account: codexAccount({ has_credits: true, unlimited: true, balance: null }),
    provider: "codex",
  })
  expect(unlimited).toContain(">Unlimited<")

  // Credits without a balance still route, so the row says they exist.
  const available = await render(AccountCard, {
    account: codexAccount({ has_credits: true, unlimited: false, balance: null }),
    provider: "codex",
  })
  expect(available).toContain(">Available<")

  for (const credits of [null, undefined, { has_credits: false, unlimited: false, balance: "0" }]) {
    const html = await render(AccountCard, { account: codexAccount(credits), provider: "codex" })
    expect(html).not.toContain('class="credits"')
  }
})

it("keeps the balance in the bars' list: muted with them on an expired sign-in, alone without them", async () => {
  const credits = { has_credits: true, unlimited: false, balance: "41.5" }
  const expiredRow = await render(AccountCard, {
    account: codexAccount(credits, { status: "reauth", error: "usage 401" }),
    provider: "codex",
  })
  expect(expiredRow).toMatch(/class="[^"]*\bfrozen\b[^"]*"[\s\S]*class="credits"/)

  const noBars = await render(AccountCard, {
    account: codexAccount(credits, { usage: null }),
    provider: "codex",
  })
  expect(noBars).toContain(">42 left<")
  expect(noBars).not.toContain("Usage will appear")

  // Neither bars nor balance: the usual empty state, not an empty list.
  const empty = await render(AccountCard, {
    account: codexAccount(null, { usage: null }),
    provider: "codex",
  })
  expect(empty).toContain("Usage will appear")
  expect(empty).not.toContain('class="windows"')
})

it("prints Antigravity's balance in the same Credits row", async () => {
  const html = await render(AccountCard, {
    account: account({ account: { email: "owner@example.com", credits_remaining: 1500 } }),
    provider: "antigravity",
  })
  expect(html).toContain(">1,500 left<")
})

it("shows a borrower the owner's balance only where the edition handed it", async () => {
  const share = { teamId: "t", teamName: "Team", ownerLabel: "Owner" }
  // A member the edition shows the account's own windows: the balance comes with them.
  const handed = await render(AccountCard, {
    account: account({
      account: null,
      share,
      usage: {
        windows: [{ label: "Week", utilization: 100, resets_at: null }],
        credits: { has_credits: true, unlimited: false, balance: "41.5" },
      },
      on_credits: true,
    }),
    provider: "codex",
    editing: true,
  })
  expect(handed).toContain(">42 left<")
  expect(handed).toContain("On credits")
  // Spending the owner's credits stays the owner's switch.
  expect(handed).not.toContain("Use credits after limits")

  // A member who sees only their own allowance gets no balance.
  const withheld = await render(AccountCard, {
    account: account({
      account: null,
      share,
      usage: { windows: [{ label: "Usage", utilization: 25, resets_at: null, value: "$2.5 / $10" }] },
    }),
    provider: "codex",
  })
  expect(withheld).toContain("$2.5 / $10")
  expect(withheld).not.toContain('class="credits"')
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

it("names a Claude Max row by its tier, as Anthropic has since the 2026-10-30 rename", async () => {
  const badge = async (meta: Record<string, unknown>) => {
    const html = await render(AccountCard, {
      account: account({ account: { email: "owner@example.com", ...meta } }),
      provider: "claude-code",
    })
    return html.match(/([^<>]+) plan</)?.[1] ?? null
  }
  // The ids did not change with the names: the old Max 20x reads Ultra, the old Max 5x reads Max.
  expect(await badge({ plan_type: "claude_max", rate_limit_tier: "default_claude_max_20x" })).toBe("Ultra")
  expect(await badge({ plan_type: "claude_max", rate_limit_tier: "default_claude_max_5x" })).toBe("Max")
  expect(await badge({ rate_limit_tier: "default_claude_max_20x" })).toBe("Ultra")
  // Team Premium shares the Max tier string and stays a Team plan.
  expect(await badge({ plan_type: "claude_team", rate_limit_tier: "default_claude_max_5x" })).toBe("Team")
  expect(await badge({ plan_type: "claude_pro" })).toBe("Pro")
  // Upstream's own name wins; a cleared one (null) falls back to the tier.
  expect(
    await badge({ plan_type: "claude_team", rate_limit_tier: "default_claude_max_5x", plan_display_name: "Team Premium" }),
  ).toBe("Team Premium")
  expect(
    await badge({ plan_type: "claude_max", rate_limit_tier: "default_claude_max_20x", plan_display_name: null }),
  ).toBe("Ultra")
})

it("renders the edition's row accessory on own rows only, with the row's ids", async () => {
  const accessory = defineComponent({
    props: { provider: String, accountId: String, label: String },
    setup: (p) => () => h("span", { class: "accessory" }, `${p.provider}:${p.accountId}:${p.label}`),
  })
  const extensions: WebExtensions = { providerRowAccessory: accessory }
  async function withExtensions(component: Component, props: Record<string, unknown>) {
    const app = createSSRApp({ render: () => h(component, props) }).provide(webExtensionsKey, extensions)
    return renderToString(app)
  }

  const own = await withExtensions(AccountCard, { account: account(), provider: "claude-code" })
  expect(own).toContain("claude-code:acct-1:owner@example.com")

  const borrowed = await withExtensions(AccountCard, {
    account: account({ share: { teamId: "t", teamName: "Team", ownerLabel: "Owner" } }),
    provider: "claude-code",
  })
  expect(borrowed).not.toContain('class="accessory"')

  const endpoint = {
    id: "cprov_1",
    slug: "gw",
    name: "Gateway",
    format: "openai",
    base_url: "https://gw.example.com/v1",
    count_tokens_url: null,
    models_mode: "auto",
    manual_models: [],
    key_mask: "sk-…a1",
    account_id: "acct-key",
    status: "active",
    strip_fast_mode: false,
    strip_claude_code_line: false,
    force_cache_ttl_1h: false,
  } as CustomProvider
  expect(await withExtensions(CustomProviderCard, { provider: endpoint })).toContain("custom:acct-key:Gateway")
  // Without its key row there is nothing for an edition to address.
  const keyless = await withExtensions(CustomProviderCard, { provider: { ...endpoint, account_id: null } })
  expect(keyless).not.toContain('class="accessory"')

  // The standalone build passes none, and the row grows nothing.
  expect(await render(AccountCard, { account: account(), provider: "claude-code" })).not.toContain("accessory")
})
