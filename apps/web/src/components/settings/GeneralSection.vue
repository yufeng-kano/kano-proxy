<script setup lang="ts">
/** Settings › General (docs/admin-ui.md § Settings dialog). */
import { computed } from "vue"
import { useI18n } from "@/i18n"
import { resetPrefs } from "@/services/prefs"
import { setTheme, themeChoice, type ThemeChoice } from "@/services/theme"
import AppButton from "@/components/ui/AppButton.vue"
import Segmented from "@/components/ui/Segmented.vue"
import SettingsGroup from "./SettingsGroup.vue"
import SettingsRow from "./SettingsRow.vue"

const { t } = useI18n()

const themeOptions = computed(() => [
  { value: "system", label: t("settings.theme.system") },
  { value: "light", label: t("settings.theme.light") },
  { value: "dark", label: t("settings.theme.dark") },
])

/**
 * Every page read its preferences when it mounted, so forgetting them only
 * shows after a reload. The URL keeps `?settings=general`, so the dialog
 * comes back open where the button was.
 */
function onReset() {
  resetPrefs()
  window.location.reload()
}
</script>

<template>
  <SettingsGroup :heading="t('settings.appearance')">
    <SettingsRow :label="t('settings.theme')" :description="t('settings.themeHint')">
      <Segmented
        :model-value="themeChoice"
        :options="themeOptions"
        :label="t('settings.theme')"
        size="sm"
        @update:model-value="setTheme($event as ThemeChoice)"
      />
    </SettingsRow>
  </SettingsGroup>

  <SettingsGroup :heading="t('settings.thisBrowser')">
    <SettingsRow :label="t('settings.resetViews')" :description="t('settings.resetViewsHint')">
      <AppButton size="sm" @click="onReset">{{ t("settings.reset") }}</AppButton>
    </SettingsRow>
    <template #note>{{ t("settings.browserNote") }}</template>
  </SettingsGroup>
</template>
