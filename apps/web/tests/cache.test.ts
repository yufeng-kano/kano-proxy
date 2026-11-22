import { beforeEach, expect, it, vi } from "vitest"
import { clearDataCaches } from "@/services/cache"

// docs/admin-ui.md § UX rules: sign-out clears what that user's session
// fetched, never another user's entries or the view preferences.

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

const entries = (user: string) => [
  `kano-proxy:accounts:${user}:codex`,
  `kano-proxy:models:${user}`,
  `kano-proxy:custom-providers:${user}`,
  `kano-proxy:model-groups:${user}`,
  `kano-proxy:cli:${user}`,
  `kano-proxy:usage:${user}:day:2026-11-15`,
  `kano-proxy:logs:${user}`,
]

function seed() {
  for (const key of [...entries("u1"), ...entries("u10"), "kano-proxy:changelog", "kano-proxy:prefs", "kano-proxy:theme"]) {
    store.set(key, "{}")
  }
}

it("clears one user's entries and the changelog, and leaves the rest", () => {
  seed()
  clearDataCaches("u1")
  // "u10" shares "u1" as a prefix and is someone else.
  expect([...store.keys()].sort()).toEqual([...entries("u10"), "kano-proxy:prefs", "kano-proxy:theme"].sort())
})

it("without a user, clears every data entry but keeps the preferences", () => {
  seed()
  clearDataCaches()
  expect([...store.keys()].sort()).toEqual(["kano-proxy:prefs", "kano-proxy:theme"])
})
