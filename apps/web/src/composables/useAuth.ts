import { computed, ref } from "vue"
import { fetchMe, loginUrl, logout as apiLogout } from "@/services/api"

const LOGIN_REDIRECT_KEY = "kano-proxy:login-redirect"

/** One-shot read of the stashed post-login target — consumed by the router's boot navigation. */
export function takeLoginRedirect(): string | null {
  try {
    const value = sessionStorage.getItem(LOGIN_REDIRECT_KEY)
    if (value) sessionStorage.removeItem(LOGIN_REDIRECT_KEY)
    return value && value.startsWith("/") ? value : null
  } catch {
    return null
  }
}
import { clearDataCaches } from "@/services/cache"
import { clearNavigationPrefs } from "@/services/prefs"
import type { User } from "@/types"

const user = ref<User | null>(null)
const loading = ref(true)
const error = ref<string | null>(null)
let bootstrapped = false

export function useAuth() {
  const isAuthenticated = computed(() => !!user.value)

  async function refresh() {
    loading.value = true
    error.value = null
    try {
      user.value = await fetchMe()
    } catch (e) {
      user.value = null
      error.value = e instanceof Error ? e.message : "Failed to load session"
    } finally {
      loading.value = false
    }
    return user.value
  }

  async function ensureSession() {
    if (!bootstrapped) {
      bootstrapped = true
      await refresh()
    }
    return user.value
  }

  async function logout() {
    try {
      await apiLogout()
    } finally {
      clearDataCaches(user.value?.id)
      // Where the user was is theirs; the impersonal view choices (range,
      // chart view) survive sign-out. See docs/admin-ui.md § View preferences.
      clearNavigationPrefs()
      user.value = null
    }
  }

  /**
   * The OAuth callback always lands on the SPA root, so the guard's
   * `?redirect=` target would be lost across the round trip — a signed-out
   * user opening /cli/authorize?request=… must come back to that exact URL
   * to approve the pending CLI login (docs/cli.md § Web UI). sessionStorage
   * is per-tab, which is exactly the scope of one sign-in.
   */
  function goLogin(redirectTo?: string | null) {
    try {
      if (redirectTo && redirectTo.startsWith("/")) {
        sessionStorage.setItem(LOGIN_REDIRECT_KEY, redirectTo)
      } else {
        sessionStorage.removeItem(LOGIN_REDIRECT_KEY)
      }
    } catch {
      /* private mode — the user just lands on the root */
    }
    window.location.href = loginUrl()
  }

  return {
    user,
    loading,
    error,
    isAuthenticated,
    refresh,
    ensureSession,
    logout,
    goLogin,
  }
}
