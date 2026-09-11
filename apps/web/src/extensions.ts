import { inject, type Component, type InjectionKey } from "vue"
import type { RouteRecordRaw } from "vue-router"

export interface NavigationItem {
  name: string
  to: string
  label: string
  icon: Component
}

export interface WebExtensions {
  routes?: RouteRecordRaw[]
  navigation?: readonly NavigationItem[]
}

export const webExtensionsKey: InjectionKey<WebExtensions> = Symbol("webExtensions")
export function useWebExtensions(): WebExtensions {
  return inject(webExtensionsKey, {})
}
