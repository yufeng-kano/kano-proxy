<script setup lang="ts">
import { computed } from "vue"
import { useRoute, useRouter } from "vue-router"
import { useAuth } from "@/composables/useAuth"
import { SITE } from "@/config/site"

const route = useRoute()
const router = useRouter()
const { user, loading, logout, isAuthenticated } = useAuth()

const showShell = computed(
  () => isAuthenticated.value && route.name !== "login",
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
