<script setup lang="ts">
/** Settings › Account (docs/admin-ui.md § Settings dialog). */
import { computed } from "vue"
import { useRouter } from "vue-router"
import { useAuth } from "@/composables/useAuth"
import { useWebExtensions } from "@/extensions"
import { useI18n } from "@/i18n"
import AppButton from "@/components/ui/AppButton.vue"
import SettingsGroup from "./SettingsGroup.vue"
import SettingsRow from "./SettingsRow.vue"

const { t, format } = useI18n()
const router = useRouter()
const { user, notice, logout } = useAuth()
const { noticeTerms = [] } = useWebExtensions()

const name = computed(() => user.value?.name?.trim() || "")

/** `YYYY-MM-DD` as a local calendar day, the way the notice page prints it. */
const noticeDate = computed(() => {
  if (!notice.value?.updated) return ""
  const [year, month, day] = notice.value.updated.split("-").map(Number)
  return format.date(new Date(year!, month! - 1, day!))
})

async function onSignOut() {
  await logout()
  await router.push({ name: "login" })
}
</script>

<template>
  <SettingsGroup>
    <SettingsRow>
      <template #label>
        <div class="profile">
          <img v-if="user?.picture_url" :src="user.picture_url" alt="" class="photo" referrerpolicy="no-referrer" />
          <span v-else class="photo photo-fallback" aria-hidden="true">{{ (name[0] ?? "?").toUpperCase() }}</span>
          <div class="profile-text">
            <div v-if="name" class="profile-name">{{ name }}</div>
            <div class="profile-email">{{ user?.email }}</div>
          </div>
        </div>
      </template>
      <span class="provider">{{ t("settings.signedInWithGoogle") }}</span>
    </SettingsRow>
  </SettingsGroup>

  <SettingsGroup v-if="notice?.accepted" :heading="t('settings.notice')">
    <SettingsRow :label="t('settings.noticeAgreed', { date: noticeDate })" :description="t('settings.noticeHint')">
      <a href="/docs/guide/risks" target="_blank" rel="noopener">{{ t("settings.risks") }}</a>
      <a v-for="link in noticeTerms" :key="link.href" :href="link.href" target="_blank" rel="noopener">{{ link.label }}</a>
    </SettingsRow>
  </SettingsGroup>

  <SettingsGroup>
    <SettingsRow :label="t('app.signOut')" :description="t('settings.signOutHint')">
      <AppButton size="sm" @click="onSignOut">{{ t("app.signOut") }}</AppButton>
    </SettingsRow>
  </SettingsGroup>
</template>

<style scoped>
.profile {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  min-width: 0;
}

.photo {
  width: 44px;
  height: 44px;
  flex-shrink: 0;
  border-radius: var(--radius);
  border: 1px solid var(--border);
  object-fit: cover;
}

.photo-fallback {
  display: grid;
  place-items: center;
  background: var(--surface);
  color: var(--muted);
  font-size: var(--text-md);
  font-weight: var(--weight-semibold);
}

.profile-text {
  min-width: 0;
}

.profile-name {
  font-size: var(--text-md);
  font-weight: var(--weight-semibold);
}

.profile-email {
  margin-top: 2px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-sm);
  color: var(--muted);
}

.provider {
  font-size: var(--text-xs);
  color: var(--faint);
}

a {
  font-size: var(--text-sm);
  color: var(--text-secondary);
  text-decoration: underline;
  text-underline-offset: 2px;
  white-space: nowrap;
}

a:hover {
  color: var(--text);
}
</style>
