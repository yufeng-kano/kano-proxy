import { beforeEach, expect, it, vi } from "vitest"
import { createMemoryHistory, type RouteRecordRaw } from "vue-router"
import { createAppRouter } from "@/router"
import { readPrefs, setLastPath } from "@/services/prefs"

// A signed-in session without a server, and the landing page without its
// component tree: the test is about which path the router records and replays.
vi.mock("@/composables/useAuth", () => ({
  useAuth: () => ({ ensureSession: async () => null, isAuthenticated: { value: true } }),
  takeLoginRedirect: () => null,
}))
vi.mock("@/pages/OverviewPage.vue", () => ({ default: { render: () => null } }))

const page = { render: () => null }
// An edition's authorize page only works with its one-shot query; a plain
// extension page has no such need.
const extensionRoutes: RouteRecordRaw[] = [
  { path: "/connect", component: page, meta: { bare: true, restorable: false } },
  { path: "/team", component: page },
]

beforeEach(() => {
  const store = new Map<string, string>()
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  })
  vi.stubGlobal("document", { title: "" })
})

/** A fresh page load on the bare app root, the only navigation that replays lastPath. */
async function boot() {
  const router = createAppRouter(extensionRoutes, { history: createMemoryHistory() })
  await router.push("/")
  return router
}

it("never records an extension route that opts out of restore", async () => {
  const router = await boot()
  await router.push("/team")
  expect(readPrefs().lastPath).toBe("/team")
  await router.push("/connect?code=one-shot")
  expect(router.currentRoute.value.path).toBe("/connect")
  expect(readPrefs().lastPath).toBe("/team")
})

it("never replays an opted-out route, even from a pref written before it opted out", async () => {
  setLastPath("/connect")
  expect((await boot()).currentRoute.value.path).toBe("/overview")

  setLastPath("/team")
  expect((await boot()).currentRoute.value.path).toBe("/team")
})
