import { afterEach, beforeEach, expect, it, vi } from "vitest"
import { useUsage } from "@/composables/useUsage"
import { readPrefs } from "@/services/prefs"

// The Overview range either follows today or is pinned to a past period
// (docs/admin-ui.md § Overview page, Following today). These pin what is
// stored and what a reload or a passed midnight lands on.
vi.mock("@/services/api", () => ({
  getUsageSummary: async () => ({ totals: { requests: 0 }, models: [], series: [] }),
}))

let store: Map<string, string>

beforeEach(() => {
  store = new Map()
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  })
  vi.useFakeTimers()
  vi.setSystemTime(new Date(2026, 10, 15, 23, 59, 0))
  // useUsage registers its midnight timer in onMounted; called outside a
  // component, Vue only warns, and the refresh path is what these exercise.
  vi.spyOn(console, "warn").mockImplementation(() => {})
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

it("ignores an anchor written before follow-today and lands on today", () => {
  store.set(
    "kano-proxy:prefs",
    JSON.stringify({ overview: { rangeKind: "day", days: 1, anchor: "2026-11-14" } }),
  )
  const usage = useUsage()
  expect(usage.range.value.anchor).toBe("2026-11-15")
  expect(readPrefs().overview.pinnedAnchor).toBeNull()
})

it("a selection covering today follows it across midnight and a reload", async () => {
  const usage = useUsage()
  usage.setRangeKind("day")
  expect(readPrefs().overview.pinnedAnchor).toBeNull()

  vi.setSystemTime(new Date(2026, 10, 16, 0, 0, 5))
  await usage.refresh()
  expect(usage.range.value.anchor).toBe("2026-11-16")

  expect(useUsage().range.value.anchor).toBe("2026-11-16")
})

it("a past period stays pinned across midnight and a reload", async () => {
  const usage = useUsage()
  usage.setRangeKind("day")
  usage.setDate(new Date(2026, 10, 10))
  expect(readPrefs().overview.pinnedAnchor).toBe("2026-11-10")

  vi.setSystemTime(new Date(2026, 10, 16, 0, 0, 5))
  await usage.refresh()
  expect(usage.range.value.anchor).toBe("2026-11-10")
  expect(useUsage().range.value.anchor).toBe("2026-11-10")
})

it("picking today again in the calendar unpins", () => {
  const usage = useUsage()
  usage.setRangeKind("day")
  usage.setDate(new Date(2026, 10, 10))
  usage.setDate(new Date(2026, 10, 15, 9))
  expect(readPrefs().overview.pinnedAnchor).toBeNull()
})

it("switching granularity re-evaluates: a pinned day inside this month follows as Month", async () => {
  const usage = useUsage()
  usage.setRangeKind("day")
  usage.setDate(new Date(2026, 10, 10))
  usage.setRangeKind("month")
  expect(usage.range.value.anchor).toBe("2026-11")
  expect(readPrefs().overview.pinnedAnchor).toBeNull()

  vi.setSystemTime(new Date(2026, 11, 1, 0, 0, 5))
  await usage.refresh()
  expect(usage.range.value.anchor).toBe("2026-12")
})

it("a followed week rolls to the next week on Monday", async () => {
  const usage = useUsage()
  usage.setRangeKind("week")
  expect(usage.range.value.anchor).toBe("2026-11-09")

  vi.setSystemTime(new Date(2026, 10, 16, 0, 0, 5))
  await usage.refresh()
  expect(usage.range.value.anchor).toBe("2026-11-16")
})
