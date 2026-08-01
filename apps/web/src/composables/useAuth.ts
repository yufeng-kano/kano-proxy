import { computed, ref } from "vue"
import { fetchMe, loginUrl, logout as apiLogout } from "@/services/api"
import { clearAccountsCache } from "@/services/cache"
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
      clearAccountsCache(user.value?.id)
      user.value = null
    }
  }

  function goLogin() {
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
