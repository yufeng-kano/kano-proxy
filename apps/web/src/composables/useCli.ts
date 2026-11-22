import { reactive, ref } from "vue"
import { listCliDevices, listCliProviders } from "@/services/api"
import {
  isCliCacheFresh,
  readCliCache,
  writeCliCache,
} from "@/services/cache"
import type { CliDevice, CliProvider, RequestEdits } from "@/types"

type CliState = {
  devices: CliDevice[] | null
  providers: CliProvider[] | null
  loading: boolean
  error: string | null
}

// Module-level singleton, same pattern as useCustomProviders: state is shared
// across every component that calls this composable.
const state = reactive<CliState>({
  devices: null,
  providers: null,
  loading: false,
  error: null,
})

// Shared with the state it scopes: the Access page's Devices tab reloads
// through its own call after a rename or revoke and never sets an id, so a
// per-call id would leave that reload out of the signed-in user's cache.
const userId = ref<string | null>(null)

export function useCli() {
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
      state.loading = false
    } else if (!state.devices) {
      state.loading = true
    }

    if (!force && isCliCacheFresh(userId.value)) {
      state.loading = false
      return
    }

    state.error = null
    try {
      const [devices, providers] = await Promise.all([listCliDevices(), listCliProviders()])
      state.devices = devices
      state.providers = providers
      writeCliCache(userId.value, { devices, providers })
    } catch (e) {
      state.error = e instanceof Error ? e.message : "Failed to load CLI devices"
      // keep cache on failure
    } finally {
      state.loading = false
    }
  }

  /** Same as useCustomProviders' `setEdits`, for one CLI provider's row. */
  function setEdits(id: string, edits: RequestEdits) {
    const row = state.providers?.find((p) => p.id === id)
    if (!row || !state.providers || !state.devices) return
    row.strip_fast_mode = edits.strip_fast_mode
    row.strip_claude_code_line = edits.strip_claude_code_line
    row.force_cache_ttl_1h = edits.force_cache_ttl_1h
    writeCliCache(userId.value, { devices: state.devices, providers: state.providers })
  }

  return { state, setUserId, load, setEdits }
}
