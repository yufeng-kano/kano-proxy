/**
 * The light / dark choice (docs/admin-ui.md § Theme).
 *
 * `<html data-theme>` always holds the resolved theme, and styles.css keys its
 * dark tokens off that attribute alone. index.html resolves it once before the
 * first paint; from boot on this module owns it.
 *
 * Its own `localStorage` key rather than a field of `kano-proxy:prefs`, so
 * Settings' "Reset remembered views" (which deletes the prefs entry) leaves it.
 */
import { ref } from "vue"

export type ThemeChoice = "system" | "light" | "dark"

const THEME_KEY = "kano-proxy:theme"

function readChoice(): ThemeChoice {
  try {
    const stored = localStorage.getItem(THEME_KEY)
    return stored === "light" || stored === "dark" ? stored : "system"
  } catch {
    return "system"
  }
}

export const themeChoice = ref<ThemeChoice>(typeof localStorage === "undefined" ? "system" : readChoice())

const darkQuery = typeof matchMedia === "undefined" ? null : matchMedia("(prefers-color-scheme: dark)")

function apply() {
  if (typeof document === "undefined") return
  const choice = themeChoice.value
  document.documentElement.dataset.theme = choice === "system" ? (darkQuery?.matches ? "dark" : "light") : choice
}

/** Called once at boot: applies the stored choice and follows the device while on System. */
export function initTheme() {
  apply()
  darkQuery?.addEventListener("change", () => {
    if (themeChoice.value === "system") apply()
  })
}

export function setTheme(choice: ThemeChoice) {
  themeChoice.value = choice
  try {
    if (choice === "system") localStorage.removeItem(THEME_KEY)
    else localStorage.setItem(THEME_KEY, choice)
  } catch {
    /* private mode: the choice still holds for this tab */
  }
  apply()
}
