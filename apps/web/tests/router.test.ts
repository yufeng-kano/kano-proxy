import { beforeEach, expect, it, vi } from "vitest"
import { createMemoryHistory, type RouteRecordRaw } from "vue-router"
import { createAppRouter, type RouterOptions } from "@/router"
import { readPrefs, setLastPath } from "@/services/prefs"

// A session without a server, signed in unless a test signs out, and the pages
// without their component trees: the tests are about which path the router
// records, replays and redirects to.
const session = vi.hoisted(() => ({ signedIn: true }))
vi.mock("@/composables/useAuth", () => ({
  useAuth: () => ({ ensureSession: async () => null, isAuthenticated: { value: session.signedIn } }),
  takeLoginRedirect: () => null,
}))
vi.mock("@/pages/OverviewPage.vue", () => ({ default: { render: () => null } }))
vi.mock("@/pages/LoginPage.vue", () => ({ default: { render: () => null } }))

const page = { render: () => null }
// An edition's authorize page only works with its one-shot query; a plain
// extension page has no such need.
const extensionRoutes: RouteRecordRaw[] = [
  { path: "/connect", component: page, meta: { bare: true, restorable: false } },
  { path: "/team", component: page },
  // An edition's landing page: public, outside the shell, never restored.
  { path: "/home", component: page, meta: { public: true, bare: true, restorable: false } },
]

beforeEach(() => {
  session.signedIn = true
  const store = new Map<string, string>()
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  })
  vi.stubGlobal("document", { title: "" })
})

/**
 * A fresh page load, on the bare app root unless `path` says otherwise. The
 * router reads the loaded URL off `window.location`, which a test supplies.
 */
async function boot(options: Omit<RouterOptions, "history"> = {}, path = "/") {
  vi.stubGlobal("window", { location: { pathname: new URL(path, "https://app.test").pathname } })
  const router = createAppRouter(extensionRoutes, { ...options, history: createMemoryHistory() })
  await router.push(path)
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

it("meets a signed-out visitor on the bare root with the edition's landing page", async () => {
  session.signedIn = false
  expect((await boot({ landing: "/home" })).currentRoute.value.path).toBe("/home")
  // The standalone build has no landing page: the root still means sign-in.
  expect((await boot()).currentRoute.value.fullPath).toBe("/login?redirect=/overview")
})

it("still sends a signed-out deep link through sign-in, landing page or not", async () => {
  session.signedIn = false
  expect((await boot({ landing: "/home" }, "/team")).currentRoute.value.fullPath).toBe("/login?redirect=/team")
})

it("keeps a signed-in root in the app, with the landing page one link away and never restored", async () => {
  const router = await boot({ landing: "/home" })
  expect(router.currentRoute.value.path).toBe("/overview")
  await router.push("/home")
  expect(router.currentRoute.value.path).toBe("/home")
  expect(readPrefs().lastPath).toBe("/overview")
})
