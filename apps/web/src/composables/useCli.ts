import { reactive, ref } from "vue"
import { listCliDevices, listCliProviders } from "@/services/api"
import {
  CACHE_TTL_MS,
  isCliCacheFresh,
  readCliCache,
  writeCliCache,
} from "@/services/cache"
import type { CliDevice, CliProvider } from "@/types"

type CliState = {
  devices: CliDevice[] | null
  providers: CliProvider[] | null
  loading: boolean
  refreshing: boolean
  error: string | null
  fromCache: boolean
}

// Module-level singleton, same pattern as useCustomProviders: state is shared
// across every component that calls this composable.
const state = reactive<CliState>({
  devices: null,
  providers: null,
  loading: false,
  refreshing: false,
  error: null,
  fromCache: false,
})

export function useCli() {
  const userId = ref<string | null>(null)

  function setUserId(id: string | null) {
    userId.value = id
  }

  /** Cache-first load — paint localStorage immediately, fetch when stale or forced. */
  async function load(opts?: { refresh?: boolean }) {
    const force = !!opts?.refresh

    const cached = readCliCache(userId.value)
    if (cached) {
      state.devices = cached.devices
      state.providers = cached.providers
      state.fromCache = true
      state.loading = false
    } else if (!state.devices) {
      state.loading = true
    }

    if (!force && isCliCacheFresh(userId.value, CACHE_TTL_MS)) {
      state.loading = false
      state.refreshing = false
      return
    }

    state.refreshing = true
    state.error = null
    try {
      const [devices, providers] = await Promise.all([listCliDevices(), listCliProviders()])
      state.devices = devices
      state.providers = providers
      state.fromCache = false
      writeCliCache(userId.value, { devices, providers })
    } catch (e) {
      state.error = e instanceof Error ? e.message : "Failed to load CLI devices"
      // keep cache on failure
    } finally {
      state.loading = false
      state.refreshing = false
    }
  }

  return { state, setUserId, load, CACHE_TTL_MS }
}
