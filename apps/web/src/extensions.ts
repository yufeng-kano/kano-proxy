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

/**
 * Shell chrome an edition may reshape. Every field defaults to the standalone
 * layout (docs/admin-ui.md § Layout: the shell).
 */
export interface ShellOptions {
  /**
   * `false` removes the Changelog link, the version badge and the `/changelog`
   * route, and the shell never asks `/api/changelog`. For an edition whose
   * release notes are not written for the people signed in to it.
   */
  changelog?: boolean
  /** Where the Documentation link lives. `"sidebar"` is the standalone default. */
  docs?: "sidebar" | "accountMenu"
}

/** A link in the sign-in page's footer, after Documentation: an edition's terms or privacy notice. */
export interface FooterLink {
  label: string
  href: string
}

export interface WebExtensions {
  routes?: RouteRecordRaw[]
  navigation?: readonly NavigationItem[]
  accountMenu?: readonly AccountMenuItem[]
  shell?: ShellOptions
  /**
   * Path of a `public` route in `routes`, the edition's landing page. A
   * signed-out visitor on the bare root lands there instead of on sign-in;
   * deep links still go through sign-in (docs/cloud-edition.md § Web shell).
   */
  landing?: string
  loginLinks?: readonly FooterLink[]
}

export const webExtensionsKey: InjectionKey<WebExtensions> = Symbol("webExtensions")
export function useWebExtensions(): WebExtensions {
  return inject(webExtensionsKey, {})
}
