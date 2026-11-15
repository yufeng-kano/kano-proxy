<script setup lang="ts">
/**
 * One custom endpoint another user shares with the viewer, after their own
 * endpoints in the custom section (docs/admin-ui.md § Providers page "Shared
 * endpoints").
 *
 * Read-only by construction: it takes no edit gate, because nothing on it is
 * the viewer's to change (every write answers 404), and the API never sends
 * what would locate or unlock it — no base URL, no key mask. What it shows is
 * the identity, the prefix to call, who lends it, the owner's request edits
 * as tags, and whatever bars the edition hands this viewer.
 */
import { computed } from "vue"
import Badge from "@/components/ui/Badge.vue"
import StatusDot from "@/components/ui/StatusDot.vue"
import UsageBar from "@/components/ui/UsageBar.vue"
import { useI18n } from "@/i18n"
import type { SharedCustomProvider } from "@/types"

const props = defineProps<{ provider: SharedCustomProvider }>()

const { t } = useI18n()

const formatLabel = computed(() =>
  props.provider.format === "anthropic"
    ? t("custom.dialog.formatAnthropic")
    : t("custom.dialog.formatOpenAI"),
)

const sharedBy = computed(() =>
  t("providers.account.shared", { owner: props.provider.share.ownerLabel }),
)

const windows = computed(() => props.provider.usage?.windows ?? [])
</script>

<template>
  <div class="endpoint">
    <div class="identity">
      <span class="name" :title="provider.name">{{ provider.name }}</span>
      <div class="tags">
        <!-- The viewer's own prefix wins every request, so this row's own
             status says nothing about where their traffic goes. -->
        <span v-if="provider.shadowed" class="unavailable">
          <span class="dot" aria-hidden="true" />{{ t("custom.shared.unavailable") }}
        </span>
        <StatusDot v-else :status="provider.status" />
        <Badge>{{ formatLabel }}</Badge>
        <Badge v-if="provider.strip_fast_mode">{{ t("providers.edits.fastModeOn") }}</Badge>
        <Badge v-if="provider.strip_claude_code_line">{{ t("providers.edits.lineOn") }}</Badge>
        <!-- Same capped pill as a shared account row: the owner name is
             open-ended text, kept whole in the title. -->
        <Badge class="share" :title="sharedBy">
          <span class="share-owner">{{ sharedBy }}</span>
        </Badge>
      </div>
    </div>

    <p v-if="provider.shadowed" class="reason">{{ t("custom.shared.shadowed") }}</p>

    <dl class="details">
      <div class="detail">
        <dt>{{ t("custom.field.modelId") }}</dt>
        <dd><code class="mono">{{ `${provider.slug}/*` }}</code></dd>
      </div>
    </dl>

    <div v-if="windows.length" class="windows">
      <UsageBar v-for="(w, i) in windows" :key="i" :window="w" />
    </div>
  </div>
</template>

<style scoped>
.endpoint {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
  padding: var(--space-4) 0;
}

.endpoint + .endpoint {
  border-top: 1px solid var(--border);
}

.identity {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  min-width: 0;
}

.name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
}

.tags {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
}

/* StatusDot's own shape, in its neutral tone: never a color alone, the word
   carries the state. */
.unavailable {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
  color: var(--text-secondary);
  white-space: nowrap;
}

.unavailable .dot {
  width: 7px;
  height: 7px;
  border-radius: var(--radius-full);
  background: var(--faint);
}

.share {
  max-width: min(100%, 16rem);
  overflow: hidden;
}

.share-owner {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.reason {
  margin: 0;
  color: var(--muted);
  font-size: var(--text-xs);
}

.details {
  display: grid;
  gap: var(--space-2);
  margin: 0;
}

.detail {
  display: grid;
  grid-template-columns: 96px minmax(0, 1fr);
  align-items: baseline;
  gap: var(--space-3);
}

.detail dt {
  color: var(--muted);
  font-size: var(--text-2xs);
}

.detail dd {
  margin: 0;
  min-width: 0;
  font-size: var(--text-xs);
}

/* The bars' columns, as on an account row: UsageBar joins them as a subgrid. */
.windows {
  display: grid;
  grid-template-columns: 78px minmax(0, 1fr) minmax(44px, auto);
  gap: var(--space-3);
}

@media (max-width: 640px) {
  .detail {
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-1);
  }

  .windows {
    grid-template-columns: 68px minmax(0, 1fr) minmax(40px, auto);
    column-gap: var(--space-2);
  }
}
</style>
