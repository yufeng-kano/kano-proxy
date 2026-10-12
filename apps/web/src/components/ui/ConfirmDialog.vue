<script setup lang="ts">
/**
 * The one confirm dialog (docs/admin-ui.md § Component primitives), driven by
 * `useConfirm()`. Mounted once by `App.vue`; a `Modal` in the alert role, so
 * it keeps the focus trap, Escape and the phone bottom sheet, and opens with
 * focus on Cancel — Enter on open never destroys anything.
 */
import { watch } from "vue"
import { useRoute } from "vue-router"
import { useI18n } from "@/i18n"
import { pendingConfirm, settleConfirm } from "@/composables/useConfirm"
import AppButton from "./AppButton.vue"
import Modal from "./Modal.vue"

const { t } = useI18n()
const route = useRoute()

// The action it guards belongs to the page being left.
watch(() => route.fullPath, () => settleConfirm(false))
</script>

<template>
  <Modal
    v-if="pendingConfirm"
    :title="pendingConfirm.title"
    alert
    @close="settleConfirm(false)"
  >
    <p class="message">{{ pendingConfirm.message }}</p>
    <template #footer>
      <AppButton @click="settleConfirm(false)">{{ t("action.cancel") }}</AppButton>
      <AppButton
        :variant="pendingConfirm.tone === 'default' ? 'primary' : 'destructive'"
        @click="settleConfirm(true)"
      >
        {{ pendingConfirm.confirmLabel }}
      </AppButton>
    </template>
  </Modal>
</template>

<style scoped>
.message {
  margin: 0;
  color: var(--text-secondary);
  line-height: 1.55;
  overflow-wrap: anywhere;
}
</style>
