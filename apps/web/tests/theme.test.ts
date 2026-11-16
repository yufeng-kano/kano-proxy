import { beforeEach, expect, it, vi } from "vitest"

// docs/admin-ui.md § Theme: <html data-theme> always holds the resolved theme,
// a forced choice outranks the device, System follows the device live, and
// "Reset remembered views" never takes the theme with it.

let store: Map<string, string>
let root: { dataset: Record<string, string> }
let deviceDark: boolean
let onDeviceChange: (() => void) | null

beforeEach(() => {
  vi.resetModules()
  store = new Map()
  root = { dataset: {} }
  deviceDark = false
  onDeviceChange = null
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  })
  // createElement: Vue's DOM runtime probes it when the module loads.
  vi.stubGlobal("document", { documentElement: root, createElement: () => ({}) })
  vi.stubGlobal("matchMedia", () => ({
    get matches() {
      return deviceDark
    },
    addEventListener: (_: string, listener: () => void) => {
      onDeviceChange = listener
    },
  }))
})

function flipDevice(dark: boolean) {
  deviceDark = dark
  onDeviceChange?.()
}

it("on System, resolves from the device and follows it while open", async () => {
  const { initTheme } = await import("@/services/theme")
  initTheme()
  expect(root.dataset.theme).toBe("light")
  flipDevice(true)
  expect(root.dataset.theme).toBe("dark")
})

it("a stored choice outranks the device, at boot and after the device changes", async () => {
  store.set("kano-proxy:theme", "light")
  deviceDark = true
  const { initTheme, themeChoice } = await import("@/services/theme")
  initTheme()
  expect(themeChoice.value).toBe("light")
  expect(root.dataset.theme).toBe("light")
  flipDevice(false)
  flipDevice(true)
  expect(root.dataset.theme).toBe("light")
})

it("choosing applies at once and persists; System removes the key", async () => {
  const { initTheme, setTheme } = await import("@/services/theme")
  initTheme()
  setTheme("dark")
  expect(root.dataset.theme).toBe("dark")
  expect(store.get("kano-proxy:theme")).toBe("dark")
  setTheme("system")
  expect(store.has("kano-proxy:theme")).toBe(false)
  expect(root.dataset.theme).toBe("light")
})

it("an unknown stored value reads as System", async () => {
  store.set("kano-proxy:theme", "sepia")
  deviceDark = true
  const { initTheme, themeChoice } = await import("@/services/theme")
  initTheme()
  expect(themeChoice.value).toBe("system")
  expect(root.dataset.theme).toBe("dark")
})

it("resetting remembered views forgets the view preferences and keeps the theme", async () => {
  const { setTheme } = await import("@/services/theme")
  const { readPrefs, resetPrefs, setLogsPrefs } = await import("@/services/prefs")
  setTheme("dark")
  setLogsPrefs({ hidden: ["latency"] })
  expect(readPrefs().logs.hidden).toEqual(["latency"])
  resetPrefs()
  expect(readPrefs().logs.hidden).toEqual([])
  expect(store.get("kano-proxy:theme")).toBe("dark")
})
