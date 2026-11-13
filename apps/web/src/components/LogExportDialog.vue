<script setup lang="ts">
/**
 * The Logs export dialog (docs/admin-ui.md § Logs page): range, filters,
 * columns and format are chosen here, and Export downloads
 * `GET /api/logs/export` as a file. A failure stays inside the dialog.
 */
import { computed, reactive, ref } from "vue"
import AppButton from "@/components/ui/AppButton.vue"
import Banner from "@/components/ui/Banner.vue"
import Modal from "@/components/ui/Modal.vue"
import Segmented from "@/components/ui/Segmented.vue"
import { useI18n } from "@/i18n"
import { downloadLogsExport } from "@/services/api"
import {
  EXPORT_FIELDS_BY_COLUMN,
  exportRange,
  localDay,
  offsetMinutes,
  type ExportRangeKind,
  type LogColumnKey,
} from "@/services/logsView"
import type { LogModelFilter } from "@/types"

const props = defineProps<{
  provider: string | null
  modelFilter: LogModelFilter
  errorsOnly: boolean
  /** The page's filters in words, e.g. "Claude Code · claude-opus-5 · All statuses". */
  filterSummary: string
  /** The table's visible columns, in order. */
  columns: LogColumnKey[]
}>()

const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()

const today = new Date()
const weekAgo = new Date(today)
weekAgo.setDate(weekAgo.getDate() - 6)

const form = reactive({
  range: "today" as ExportRangeKind,
  from: localDay(weekAgo),
  to: localDay(today),
  useFilters: true,
  columns: "visible" as "visible" | "all",
  format: "csv" as "csv" | "json",
})
const exporting = ref(false)
const error = ref<string | null>(null)

const rangeOptions = computed(() =>
  (["today", "7d", "30d", "custom"] as const).map((value) => ({
    value,
    label: t(`logs.export.range.${value}`),
  })),
)
const formatOptions = [
  { value: "csv", label: "CSV" },
  { value: "json", label: "JSON" },
]

async function submit() {
  const range = exportRange(form.range, new Date(), { from: form.from, to: form.to })
  if (!range) {
    error.value = t("logs.export.invalidRange")
    return
  }
  exporting.value = true
  error.value = null
  try {
    await downloadLogsExport({
      ...range,
      format: form.format,
      offsetMinutes: offsetMinutes(new Date()),
      columns: form.columns === "visible" ? props.columns.flatMap((key) => EXPORT_FIELDS_BY_COLUMN[key]) : undefined,
      ...(form.useFilters
        ? { provider: props.provider ?? undefined, modelFilter: props.modelFilter, errorsOnly: props.errorsOnly }
        : {}),
    })
    emit("close")
  } catch {
    error.value = t("logs.export.failed")
  } finally {
    exporting.value = false
  }
}
</script>

<template>
  <Modal :title="t('logs.export.title')" size="sm" @close="emit('close')">
    <div class="body">
      <p class="intro">{{ t("logs.export.intro") }}</p>

      <fieldset class="group">
        <legend class="legend">{{ t("logs.export.range") }}</legend>
        <Segmented v-model="form.range" :options="rangeOptions" :label="t('logs.export.range')" />
        <div v-if="form.range === 'custom'" class="dates">
          <label class="date">
            <span>{{ t("logs.export.from") }}</span>
            <input v-model="form.from" type="date" class="date-input" :max="form.to" />
          </label>
          <label class="date">
            <span>{{ t("logs.export.to") }}</span>
            <input v-model="form.to" type="date" class="date-input" :min="form.from" />
          </label>
        </div>
      </fieldset>

      <fieldset class="group">
        <legend class="legend">{{ t("logs.export.filters") }}</legend>
        <label class="check">
          <input v-model="form.useFilters" type="checkbox" />
          <span class="check-text">
            <span class="check-title">{{ t("logs.export.useFilters") }}</span>
            <span class="check-hint">{{ filterSummary }}</span>
          </span>
        </label>
      </fieldset>

      <fieldset class="group">
        <legend class="legend">{{ t("logs.export.columns") }}</legend>
        <label class="check">
          <input v-model="form.columns" type="radio" name="export-columns" value="visible" />
          <span class="check-text">
            <span class="check-title">{{ t("logs.export.columns.visible") }}</span>
            <span class="check-hint">{{ t("logs.export.columns.visibleNote", { count: columns.length }) }}</span>
          </span>
        </label>
        <label class="check">
          <input v-model="form.columns" type="radio" name="export-columns" value="all" />
          <span class="check-text">
            <span class="check-title">{{ t("logs.export.columns.all") }}</span>
            <span class="check-hint">{{ t("logs.export.columns.allNote") }}</span>
          </span>
        </label>
      </fieldset>

      <fieldset class="group">
        <legend class="legend">{{ t("logs.export.format") }}</legend>
        <Segmented v-model="form.format" :options="formatOptions" :label="t('logs.export.format')" />
        <p class="check-hint">
          {{ form.format === "csv" ? t("logs.export.format.csvNote") : t("logs.export.format.jsonNote") }}
        </p>
      </fieldset>

      <Banner v-if="error" tone="error">{{ error }}</Banner>
    </div>

    <template #footer>
      <AppButton variant="secondary" :disabled="exporting" @click="emit('close')">
        {{ t("action.cancel") }}
      </AppButton>
      <AppButton variant="primary" :loading="exporting" @click="submit">
        {{ t("logs.export.submit") }}
      </AppButton>
    </template>
  </Modal>
</template>

<style scoped>
.body {
  display: flex;
  flex-direction: column;
  gap: var(--space-5);
}

.intro {
  margin: 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
}

.group {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  min-width: 0;
  margin: 0;
  padding: 0;
  border: none;
}

.legend {
  margin-bottom: var(--space-2);
  padding: 0;
  font-size: var(--text-xs);
  font-weight: var(--weight-semibold);
  color: var(--text-secondary);
}

.dates {
  display: flex;
  gap: var(--space-3);
}

.date {
  display: flex;
  flex: 1 1 0;
  flex-direction: column;
  gap: var(--space-1);
  min-width: 0;
  font-size: var(--text-xs);
  color: var(--muted);
}

.date-input {
  height: 34px;
  padding: 0 var(--space-2);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-control);
  background: var(--surface);
  color: var(--text);
  font: inherit;
  font-size: var(--text-sm);
}

/* The same row shape as the key dialog's checkbox. */
.check {
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
  cursor: pointer;
}

.check input {
  flex-shrink: 0;
  margin: 3px 0 0;
  accent-color: var(--accent);
}

.check-text {
  display: grid;
  gap: var(--space-1);
  min-width: 0;
}

.check-title {
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
}

.check-hint {
  margin: 0;
  color: var(--muted);
  font-size: var(--text-xs);
}
</style>
