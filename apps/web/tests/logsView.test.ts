import { describe, expect, it } from "vitest"
import {
  daysAgo,
  exportRange,
  filterKey,
  modelName,
  optionColors,
  pickerOptions,
  rowOptionKey,
} from "@/services/logsView"
import type { LogFacets, RequestLogRow } from "@/types"

const facets: LogFacets = {
  providers: [
    { provider: "claude-code", requests: 5, cost: 2 },
    { provider: "codex", requests: 1, cost: null },
  ],
  models: [
    { provider: "claude-code", model: "claude-code/opus", requests: 3, cost: 1.5 },
    { provider: "claude-code", model: "claude-code/haiku", requests: 2, cost: 0.5 },
    { provider: "codex", model: "codex/gpt-5", requests: 1, cost: null },
  ],
  groups: [{ group_name: "lincy/main", requests: 3, cost: 1.5, models: ["claude-code/opus"] }],
  direct: { requests: 3, cost: 0.5 },
}

const name = (slug: string) => slug.toUpperCase()

function row(overrides: Partial<RequestLogRow>): RequestLogRow {
  return {
    id: "log_1",
    created_at: "2026-11-13T14:00:00.000Z",
    provider: "claude-code",
    model: "claude-code/opus",
    group_name: null,
    account_id: null,
    account_label: null,
    account_removed: false,
    account_shared_by: null,
    api_key_name: null,
    api_key_removed: false,
    usage_type: "oauth",
    status_code: 200,
    upstream_status: 200,
    error_code: null,
    latency_ms: 1,
    prompt_tokens: null,
    completion_tokens: null,
    cache_read_input_tokens: null,
    cache_creation_input_tokens: null,
    cost: null,
    ...overrides,
  }
}

describe("modelName", () => {
  it("drops only its own provider's prefix", () => {
    expect(modelName("claude-code/opus", "claude-code")).toBe("opus")
    expect(modelName("other/opus", "claude-code")).toBe("other/opus")
    expect(modelName("", "unknown")).toBe("")
  })
})

describe("pickerOptions", () => {
  it("lists today's models most-used first, narrowed to the provider", () => {
    const all = pickerOptions(facets, "model", null, null, name, "No alias")
    expect(all.map((o) => o.key)).toEqual(["model:claude-code/opus", "model:claude-code/haiku", "model:codex/gpt-5"])
    expect(all[0]).toMatchObject({ label: "opus", sub: "CLAUDE-CODE", tally: { requests: 3, cost: 1.5 } })
    const codex = pickerOptions(facets, "model", "codex", null, name, "No alias")
    expect(codex.map((o) => o.key)).toEqual(["model:codex/gpt-5"])
  })

  it("lists aliases by their targets' provider and always offers no alias", () => {
    const all = pickerOptions(facets, "alias", null, null, name, "No alias")
    expect(all.map((o) => [o.key, o.label, o.tally])).toEqual([
      ["group:lincy/main", "lincy/main", { requests: 3, cost: 1.5 }],
      ["direct", "No alias", { requests: 3, cost: 0.5 }],
    ])
    // The no-alias count spans every provider, so a narrowed list shows none.
    const codex = pickerOptions(facets, "alias", "codex", null, name, "No alias")
    expect(codex.map((o) => [o.key, o.tally])).toEqual([["direct", null]])
  })

  it("keeps an active choice that has no option as a bare one, in either view", () => {
    const stale = { kind: "model", value: "grok/old" } as const
    for (const view of ["model", "alias"] as const) {
      expect(pickerOptions(facets, view, null, stale, name, "No alias").at(-1)).toMatchObject({
        key: "model:grok/old",
        label: "grok/old",
        tally: null,
      })
    }
    expect(pickerOptions(facets, "model", null, { kind: "direct" }, name, "No alias").at(-1)).toMatchObject({
      key: "direct",
      label: "No alias",
    })
  })

  it("works before the facets land", () => {
    expect(pickerOptions(null, "model", null, null, name, "No alias")).toEqual([])
    expect(pickerOptions(null, "alias", null, null, name, "No alias")).toEqual([
      { key: "direct", filter: { kind: "direct" }, label: "No alias", sub: null, tally: null },
    ])
  })
})

describe("colors", () => {
  it("hands out six series hues by order, then the neutral one, and never to no alias", () => {
    const options = Array.from({ length: 8 }, (_, i) => ({
      key: `model:m${i}`,
      filter: null,
      label: `m${i}`,
      sub: null,
      tally: { requests: 1, cost: null },
    }))
    const colors = optionColors([{ key: "direct", filter: null, label: "", sub: null, tally: { requests: 9, cost: null } }, ...options])
    expect(colors.get("direct")).toBe("var(--series-other)")
    expect(colors.get("model:m0")).toBe("var(--series-1)")
    expect(colors.get("model:m5")).toBe("var(--series-6)")
    expect(colors.get("model:m6")).toBe("var(--series-other)")
  })

  it("matches a row to its option in the active view", () => {
    const aliased = row({ group_name: "lincy/main" })
    expect(rowOptionKey(aliased, "model")).toBe("model:claude-code/opus")
    expect(rowOptionKey(aliased, "alias")).toBe("group:lincy/main")
    expect(rowOptionKey(row({}), "alias")).toBe("direct")
    expect(filterKey(null)).toBe("all")
    expect(filterKey({ kind: "direct" })).toBe(rowOptionKey(row({}), "alias"))
    expect(filterKey({ kind: "group", value: "lincy/main" })).toBe(rowOptionKey(aliased, "alias"))
  })
})

describe("local calendar", () => {
  it("counts whole local days back from now", () => {
    const now = new Date(2026, 10, 13, 0, 5)
    expect(daysAgo(new Date(2026, 10, 13, 0, 1).toISOString(), now)).toBe(0)
    expect(daysAgo(new Date(2026, 10, 12, 23, 59).toISOString(), now)).toBe(1)
    expect(daysAgo(new Date(2026, 9, 31, 12).toISOString(), now)).toBe(13)
  })

  it("builds export ranges from local midnights", () => {
    const now = new Date(2026, 10, 13, 22, 31)
    expect(exportRange("today", now)).toEqual({ from: new Date(2026, 10, 13).toISOString(), to: now.toISOString() })
    expect(exportRange("7d", now)?.from).toBe(new Date(2026, 10, 7).toISOString())
    expect(exportRange("30d", now)?.from).toBe(new Date(2026, 9, 15).toISOString())
    expect(exportRange("custom", now, { from: "2026-11-01", to: "2026-11-02" })).toEqual({
      from: new Date(2026, 10, 1).toISOString(),
      to: new Date(2026, 10, 3, 0, 0, 0, -1).toISOString(),
    })
    expect(exportRange("custom", now, { from: "2026-11-03", to: "2026-11-02" })).toBeNull()
    expect(exportRange("custom", now, { from: "", to: "2026-11-02" })).toBeNull()
  })
})
