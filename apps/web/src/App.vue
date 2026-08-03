<script setup lang="ts">
/**
 * Root: picks between the signed-in shell and the bare login surface, and
 * shows nothing but a quiet loader while the session is still resolving.
 *
 * Everything else lives in AppShell or a page — this file stays a switch.
 */
import { computed } from "vue"
import { useRoute } from "vue-router"
import AppShell from "@/components/ui/AppShell.vue"
import Spinner from "@/components/ui/Spinner.vue"
import { useAuth } from "@/composables/useAuth"
import { useI18n } from "@/i18n"

const route = useRoute()
const { t } = useI18n()
const { loading, isAuthenticated } = useAuth()

const showShell = computed(() => isAuthenticated.value && route.name !== "login")
/** Session unresolved and not on login: the router is still deciding where to go. */
const showBoot = computed(() => loading.value && !showShell.value && route.name !== "login")
</script>

<template>
  <AppShell v-if="showShell">
    <RouterView />
  </AppShell>

  <div v-else-if="showBoot" class="boot" role="status" aria-live="polite">
    <Spinner :size="18" />
    <span class="sr-only">{{ t("app.loading") }}</span>
  </div>

  <RouterView v-else />
</template>

<style scoped>
.boot {
  display: grid;
  place-items: center;
  height: 100dvh;
  color: var(--muted);
}
</style>
