<script setup lang="ts">
import { computed, watch } from "vue"
import { useRoute, useRouter } from "vue-router"
import { useAuth } from "@/composables/useAuth"
import { useChangelog } from "@/composables/useChangelog"
import { SITE } from "@/config/site"

const route = useRoute()
const router = useRouter()
const { user, loading, logout, isAuthenticated } = useAuth()
const { data: changelog, setUserId, load: loadChangelog } = useChangelog()

const showShell = computed(
  () => isAuthenticated.value && route.name !== "login",
)

/** Blank until the first load resolves, so the topbar never shows a wrong version. */
const version = computed(() => changelog.value?.current ?? "")
/** Server-computed — a local build ahead of the newest release is not an update. */
const updateAvailable = computed(() => changelog.value?.updateAvailable === true)

// The badge is part of the signed-in shell, so it loads once the session is
// known rather than on mount. The composable dedupes against the Changelog
// page's own load when both mount on the same tick.
watch(
  () => (showShell.value ? (user.value?.id ?? null) : null),
  (id) => {
    setUserId(id)
    if (id) void loadChangelog()
  },
  { immediate: true },
)

async function onLogout() {
  await logout()
  await router.push({ name: "login" })
}
</script>

<template>
  <div class="app-root">
    <header v-if="showShell" class="topbar">
      <div class="topbar-inner">
        <div class="brand">
          <span class="brand-mark">k</span>
          <span class="brand-name">{{ SITE.name }}</span>
          <RouterLink
            v-if="version"
            to="/changelog"
            class="version-badge"
            :class="{ 'has-update': updateAvailable }"
            :title="
              updateAvailable
                ? `Running v${version} — a newer release is available`
                : `Running v${version} — what's new`
            "
          >
            v{{ version }}
            <span v-if="updateAvailable" class="update-dot" aria-hidden="true"></span>
            <span v-if="updateAvailable" class="sr-only">update available</span>
          </RouterLink>
        </div>
        <nav class="nav">
          <RouterLink to="/dashboard" class="nav-link">Dashboard</RouterLink>
          <RouterLink to="/accounts" class="nav-link">Accounts</RouterLink>
          <RouterLink to="/models" class="nav-link">Models</RouterLink>
          <RouterLink to="/keys" class="nav-link">Keys</RouterLink>
        </nav>
        <div class="user-slot">
          <img
            v-if="user?.picture_url"
            :src="user.picture_url"
            alt=""
            class="avatar"
            referrerpolicy="no-referrer"
          />
          <span class="user-label">{{ user?.email || user?.name || "Account" }}</span>
          <button type="button" class="btn btn-ghost btn-sm" @click="onLogout">
            Sign out
          </button>
        </div>
      </div>
    </header>

    <main class="main" :class="{ 'main-bare': !showShell }">
      <div v-if="loading && !showShell && route.name !== 'login'" class="center-state">
        <p class="muted">Loading…</p>
      </div>
      <RouterView v-else />
    </main>
  </div>
</template>
