<script setup lang="ts">
/**
 * One request log row, in full (docs/admin-ui.md § Logs page).
 *
 * Everything the list truncates or hides on a narrow width is here as text:
 * the ids behind the resolved names, `upstream_status` — which `status_code`
 * hides for an eager stream — and the token fields that render `—` when the
 * upstream never reported them. There is no message content to show, and none
 * is fetched (docs/logging.md).
 */
import { computed } from "vue"
import Badge from "@/components/ui/Badge.vue"
import Modal from "@/components/ui/Modal.vue"
import { useI18n } from "@/i18n"
import type { RequestLogRow } from "@/types"

const props = defineProps<{ row: RequestLogRow }>()

defineEmits<{ close: [] }>()

const { t, format } = useI18n()

/** Rendered wherever a field has nothing to show, like every formatter does. */
const EM_DASH = "—"

/** A resolved name, the deleted-record note, or nothing to name at all. */
type Reference = { name: string | null; missing: boolean; id: string | null }

const account = computed<Reference>(() => ({
  name: props.row.account_label,
  missing: props.row.account_id !== null && props.row.account_label === null,
  id: props.row.account_id,
}))

const apiKey = computed<Reference>(() => ({
  name: props.row.api_key_name,
  missing: props.row.api_key_id !== null && props.row.api_key_name === null,
  id: props.row.api_key_id,
}))

const typeLabel = computed(() =>
  props.row.usage_type === "oauth" ? t("logs.type.oauth") : t("logs.type.api"),
)

/** The plain label/value rows — the ones with no tag or id beneath them. */
const fields = computed<{ key: string; label: string; value: string; mono?: boolean }[]>(() => [
  { key: "id", label: t("logs.detail.id"), value: props.row.id, mono: true },
  { key: "provider", label: t("logs.detail.provider"), value: props.row.provider },
  { key: "model", label: t("logs.column.model"), value: props.row.model, mono: true },
  { key: "alias", label: t("logs.detail.alias"), value: props.row.group_name ?? EM_DASH },
  { key: "type", label: t("logs.column.type"), value: typeLabel.value },
  { key: "status", label: t("logs.column.status"), value: String(props.row.status_code) },
  {
    key: "upstream",
    label: t("logs.detail.upstreamStatus"),
    value: props.row.upstream_status === null ? EM_DASH : String(props.row.upstream_status),
  },
  { key: "error", label: t("logs.detail.error"), value: props.row.error_code ?? EM_DASH, mono: true },
  { key: "input", label: t("logs.column.input"), value: format.integer(props.row.prompt_tokens) },
  {
    key: "cacheRead",
    label: t("logs.column.cacheRead"),
    value: format.integer(props.row.cache_read_input_tokens),
  },
  {
    key: "cacheWrite",
    label: t("logs.column.cacheWrite"),
    value: format.integer(props.row.cache_creation_input_tokens),
  },
  { key: "output", label: t("logs.column.output"), value: format.integer(props.row.completion_tokens) },
  { key: "cost", label: t("logs.column.cost"), value: format.currency(props.row.cost) },
  { key: "latency", label: t("logs.column.latency"), value: format.duration(props.row.latency_ms) },
])
</script>

<template>
  <Modal :title="t('logs.detail.title')" size="md" @close="$emit('close')">
    <dl class="fields">
      <div class="field">
        <dt>{{ t("logs.column.time") }}</dt>
        <dd :title="row.created_at">{{ format.timestamp(row.created_at) }}</dd>
      </div>

      <!-- Name, then the id it resolved from: a record deleted since the
           request ran has no name left, and the id is what identifies it in
           the evidence the operator still has. -->
      <div class="field">
        <dt>{{ t("logs.detail.account") }}</dt>
        <dd>
          <Badge v-if="account.missing" tone="warn">{{ t("logs.accountRemoved") }}</Badge>
          <span v-else-if="account.name">{{ account.name }}</span>
          <span v-else class="none">{{ EM_DASH }}</span>
          <span v-if="account.id" class="mono id">{{ account.id }}</span>
        </dd>
      </div>

      <div class="field">
        <dt>{{ t("logs.detail.apiKey") }}</dt>
        <dd>
          <Badge v-if="apiKey.missing" tone="warn">{{ t("logs.detail.keyRemoved") }}</Badge>
          <span v-else-if="apiKey.name">{{ apiKey.name }}</span>
          <span v-else class="none">{{ EM_DASH }}</span>
          <span v-if="apiKey.id" class="mono id">{{ apiKey.id }}</span>
        </dd>
      </div>

      <div v-for="field in fields" :key="field.key" class="field">
        <dt>{{ field.label }}</dt>
        <dd :class="{ mono: field.mono }">{{ field.value }}</dd>
      </div>
    </dl>
  </Modal>
</template>

<style scoped>
.fields {
  display: grid;
  gap: var(--space-3);
  margin: 0;
}

/* Label and value on one row, the label on a track sized to the longest of
   them so the values line up as a column. */
.field {
  display: grid;
  grid-template-columns: 140px minmax(0, 1fr);
  gap: var(--space-4);
  align-items: baseline;
}

.field + .field {
  padding-top: var(--space-3);
  border-top: 1px solid var(--border);
}

dt {
  color: var(--muted);
  font-size: var(--text-xs);
}

dd {
  margin: 0;
  min-width: 0;
  color: var(--text);
  font-size: var(--text-sm);
  /* Ids and long model names wrap inside the panel rather than widening it. */
  overflow-wrap: anywhere;
}

/* The id under a resolved name: present for tracing, quiet enough that the
   name is still what the row reads as. */
.id {
  display: block;
  margin-top: var(--space-1);
  color: var(--faint);
  font-size: var(--text-2xs);
}

.none {
  color: var(--faint);
}

/* One column below the phone breakpoint — a 140px label track leaves the
   values a strip too narrow to read an id in. */
@media (max-width: 640px) {
  .field {
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-1);
  }
}
</style>
