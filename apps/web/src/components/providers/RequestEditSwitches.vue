<script setup lang="ts">
/**
 * One account's request-edit switches (docs/providers.md § Request edits):
 * Remove fast mode and Remove the Claude Code line, each a labelled checkbox
 * with one line saying what it does. The same pair on a builtin account row,
 * a custom endpoint and a CLI provider (docs/admin-ui.md § Providers page).
 *
 * Presentational: whoever owns the provider's data writes the change and
 * reloads, and `busy` covers that write.
 */
import { ref, watch } from "vue"
import { useI18n } from "@/i18n"
import type { RequestEdits } from "@/types"

const props = defineProps<{
  stripFastMode: boolean
  stripClaudeCodeLine: boolean
  /** The fast-mode switch's line. Absent hides the switch: the provider never forwards fast mode. */
  fastModeHint?: string
  lineHint: string
  busy?: boolean
}>()

const emit = defineEmits<{ change: [patch: Partial<RequestEdits>] }>()

const { t } = useI18n()

/**
 * The native checkbox flips itself on click, and Vue patches nothing back
 * when the bound value never changed — so once the write settles, each box is
 * put back to whatever the props now say (a failure reverts it).
 */
const fastBox = ref<HTMLInputElement | null>(null)
const lineBox = ref<HTMLInputElement | null>(null)
watch(
  () => props.busy,
  (busy) => {
    if (busy) return
    if (fastBox.value) fastBox.value.checked = props.stripFastMode
    if (lineBox.value) lineBox.value.checked = props.stripClaudeCodeLine
  },
)
</script>

<template>
  <div class="switches">
    <label v-if="fastModeHint" class="check">
      <input
        ref="fastBox"
        type="checkbox"
        :checked="stripFastMode"
        :disabled="busy"
        @change="emit('change', { strip_fast_mode: ($event.target as HTMLInputElement).checked })"
      />
      <span class="check-text">
        <span class="check-title">{{ t("providers.edits.fastMode") }}</span>
        <span class="check-hint">{{ fastModeHint }}</span>
      </span>
    </label>
    <label class="check">
      <input
        ref="lineBox"
        type="checkbox"
        :checked="stripClaudeCodeLine"
        :disabled="busy"
        @change="
          emit('change', { strip_claude_code_line: ($event.target as HTMLInputElement).checked })
        "
      />
      <span class="check-text">
        <span class="check-title">{{ t("providers.edits.line") }}</span>
        <span class="check-hint">{{ lineHint }}</span>
      </span>
    </label>
  </div>
</template>

<style scoped>
.switches {
  display: grid;
  gap: var(--space-3);
}

.check {
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
  cursor: pointer;
}

.check input {
  flex-shrink: 0;
  width: 16px;
  height: 16px;
  margin: 2px 0 0;
  accent-color: var(--accent);
}

.check input:disabled,
.check:has(input:disabled) {
  cursor: progress;
}

.check-text {
  display: grid;
  gap: 2px;
  min-width: 0;
}

.check-title {
  font-size: var(--text-base);
  font-weight: var(--weight-medium);
}

.check-hint {
  color: var(--muted);
  font-size: var(--text-sm);
  line-height: 1.5;
}
</style>
