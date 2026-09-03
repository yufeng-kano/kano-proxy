<script setup lang="ts">
/**
 * Status indicator. The dot is never alone — it always ships with its label,
 * because color-only status fails both colorblind users and screen readers
 * (docs/admin-ui.md § Accessibility floor).
 *
 * `labelOnly` hides the text visually for dense rows where a neighbouring
 * column already spells the status out; the accessible name stays.
 */
import { computed } from "vue"
import { useI18n } from "@/i18n"
import type { AccountStatus } from "@/types"

const props = defineProps<{
  status: AccountStatus
  /** Visually hide the label, keeping it for assistive tech. */
  labelOnly?: boolean
}>()

const { t } = useI18n()

const label = computed(() => {
  switch (props.status) {
    case "active":
      return t("status.active")
    case "active_no_fable":
      return t("status.activeNoFable")
    case "active_fable":
      return t("status.activeFable")
    case "standby":
      return t("status.standby")
    case "limited":
      return t("status.limited")
    case "benched":
      return t("status.benched")
    default:
      return t("status.unusable")
  }
})
</script>

<template>
  <span class="status" :class="status">
    <span class="dot" aria-hidden="true" />
    <span :class="labelOnly ? 'sr-only' : 'label'">{{ label }}</span>
  </span>
</template>

<style scoped>
.status {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  min-width: 0;
}

.dot {
  width: 7px;
  height: 7px;
  border-radius: var(--radius-full);
  flex-shrink: 0;
  background: var(--faint);
}

.label {
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
  color: var(--text-secondary);
  white-space: nowrap;
}

.active .dot {
  background: var(--ok);
  box-shadow: var(--ok-ring);
}

/* The two Fable route states (docs/admin-ui.md § Providers page): blue is
   the general route that cannot serve Fable, purple is the row Fable
   traffic goes to instead. Both are "active" states, so both get the ring. */
.active_no_fable .dot {
  background: var(--route-partial);
  box-shadow: var(--route-partial-ring);
}

.active_fable .dot {
  background: var(--route-fable);
  box-shadow: var(--route-fable-ring);
}

.standby .dot {
  background: var(--standby);
}

/* Limited and benched share the warn tone: both mean "usable again later,
   nothing to fix". The label is what separates them — the dot never carries a
   status on its own (docs/admin-ui.md § Accessibility floor). */
.limited .dot {
  background: var(--warn);
}

.benched .dot {
  background: var(--warn);
}

.unusable .dot {
  background: var(--danger);
}
</style>
