import { inject, type Component, type InjectionKey } from "vue"
import type { RouteRecordRaw } from "vue-router"

export interface NavigationItem {
  name: string
  to: string
  label: string
  icon: Component
}

/**
 * An entry in the signed-in shell's account menu, under the user block.
 * Destinations an edition owns that are about the account rather than about
 * the proxy — the core itself adds none.
 */
export interface AccountMenuItem {
  name: string
  to: string
  label: string
  icon: Component
}

export interface WebExtensions {
  routes?: RouteRecordRaw[]
  navigation?: readonly NavigationItem[]
  accountMenu?: readonly AccountMenuItem[]
}

export const webExtensionsKey: InjectionKey<WebExtensions> = Symbol("webExtensions")
export function useWebExtensions(): WebExtensions {
  return inject(webExtensionsKey, {})
}
