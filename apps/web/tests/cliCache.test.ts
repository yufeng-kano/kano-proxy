import { beforeEach, expect, it, vi } from "vitest"
import { useCli } from "@/composables/useCli"
import { readCliCache } from "@/services/cache"
import type { CliDevice } from "@/types"

// docs/admin-ui.md § Access page: a revoked device leaves the list at once,
// through the CLI cache the Access and Providers pages share. The Devices tab
// reloads through its own useCli() call, which never sets a user id.

let store: Map<string, string>

beforeEach(() => {
  store = new Map()
  vi.stubGlobal("localStorage", {
    get length() {
      return store.size
    },
    key: (i: number) => [...store.keys()][i] ?? null,
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  })
})

const device = (id: string): CliDevice => ({
  id,
  name: id,
  last_seen_at: null,
  created_at: "2026-11-01T00:00:00.000Z",
  key_count: 0,
})

function serve(devices: CliDevice[]) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      const body = url.endsWith("/api/cli/devices") ? { devices } : { providers: [] }
      return new Response(JSON.stringify(body), { status: 200 })
    }),
  )
}

it("a reload from a caller that never set the user still lands in that user's cache", async () => {
  serve([device("laptop"), device("desktop")])
  const page = useCli()
  page.setUserId("u1")
  await page.load()
  expect(readCliCache("u1")?.devices.map((d) => d.id)).toEqual(["laptop", "desktop"])

  // The device was revoked: the section's refresh must replace the cached list,
  // or the next visit inside the TTL paints the revoked device again.
  serve([device("desktop")])
  await useCli().load({ refresh: true })
  expect(readCliCache("u1")?.devices.map((d) => d.id)).toEqual(["desktop"])
})
