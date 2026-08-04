<script setup lang="ts">
/**
 * One connected subscription account inside a provider section.
 *
 * A row rather than a card-in-a-card: the section's AppCard is already the
 * surface, so this only needs an identity line, its usage windows, and the
 * actions — which stay behind a pencil toggle (docs/admin-ui.md § Providers
 * page): a resting row shows identity and status only, so Remove is never one
 * accidental click away and a long pool is not a wall of buttons. The status
 * dot always ships its text label (docs/admin-ui.md § Accessibility floor).
 */
import { computed, ref } from "vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import Badge from "@/components/ui/Badge.vue"
import Banner from "@/components/ui/Banner.vue"
import StatusDot from "@/components/ui/StatusDot.vue"
import UsageBar from "@/components/ui/UsageBar.vue"
import { useI18n } from "@/i18n"
import type { ProviderAccount } from "@/types"

const props = defineProps<{
  account: ProviderAccount
  busy?: boolean
}>()

const emit = defineEmits<{
  promote: []
  remove: []
}>()

const { t } = useI18n()

/** Pencil-gated: the row's actions render only while this is on. */
const editing = ref(false)

/**
 * Server data, not copy: an email or upstream username identifies the account
 * to its owner. The generic provider names are rejected as identities because
 * upstream returns them as a placeholder for "no profile", where the account
 * id at least distinguishes two accounts from each other.
 */
const GENERIC_LABELS = new Set(["claude", "codex", "grok"])

const displayName = computed(() => {
  const a = props.account
  const meta = a.account ?? {}
  const email =
    (typeof meta.email === "string" && meta.email) ||
    (typeof meta.account_email === "string" && meta.account_email) ||
    null
  const username =
    (typeof meta.username === "string" && meta.username) ||
    (typeof meta.display_name === "string" && meta.display_name) ||
    (typeof meta.plan_type === "string" && meta.plan_type) ||
    null
  if (email) return email
  if (username && !GENERIC_LABELS.has(username)) return username
  if (a.label && !GENERIC_LABELS.has(a.label)) return a.label
  return email || username || a.label || a.id.slice(0, 12)
})

/**
 * Prettify raw upstream plan ids for display:
 * "claude_pro" → "Pro", "default_claude_max_20x" → "Max 20x", "plus" → "Plus".
 */
function formatPlan(raw: string): string {
  const tokens = raw
    .replace(/^SUBSCRIPTION_TIER_/i, "")
    .toLowerCase()
    .split(/[_\s-]+/)
    .filter(Boolean)
  const providerPrefixes = new Set([
    "default",
    "claude",
    "chatgpt",
    "codex",
    "grok",
    "subscription",
    "tier",
  ])
  while (tokens.length > 1 && providerPrefixes.has(tokens[0]!)) tokens.shift()
  if (!tokens.length) return raw
  const special: Record<string, string> = { supergrok: "SuperGrok" }
  return tokens
    .map((s) => (/^\d+x$/.test(s) ? s : (special[s] ?? s.charAt(0).toUpperCase() + s.slice(1))))
    .join(" ")
}

const plan = computed(() => {
  const meta = props.account.account ?? {}
  const raw =
    (typeof meta.plan_type === "string" && meta.plan_type) ||
    (typeof meta.rate_limit_tier === "string" && meta.rate_limit_tier) ||
    null
  return raw ? formatPlan(raw) : null
})

/**
 * Codex's usage API sits behind chatgpt.com's bot wall. Chat still routes
 * fine, so surfacing it would be reporting a failure the user cannot act on
 * and that is not actually breaking their requests.
 */
const usageEdgeBlocked = computed(
  () => !!props.account.error && /edge blocked|403 bot challenge/i.test(props.account.error),
)

const windows = computed(() => props.account.usage?.windows ?? [])
</script>

<template>
  <div class="account">
    <div class="account-head">
      <div class="identity">
        <span class="name" :title="displayName">{{ displayName }}</span>
        <div class="tags">
          <StatusDot :status="account.status" />
          <!-- `active` *is* the account requests route through first, so this
               closes the loop on the "Make primary" button: the word the user
               pressed is the word they get back. -->
          <Badge v-if="account.status === 'active'" tone="accent">
            {{ t("providers.account.primary") }}
          </Badge>
          <Badge v-if="plan">{{ t("providers.account.plan", { plan }) }}</Badge>
        </div>
      </div>

      <div class="actions">
        <template v-if="editing">
          <AppButton
            v-if="account.status !== 'active'"
            size="sm"
            :disabled="busy"
            @click="emit('promote')"
          >
            {{ t("providers.account.promote") }}
          </AppButton>
          <AppButton size="sm" variant="danger" :disabled="busy" @click="emit('remove')">
            {{ t("providers.account.remove") }}
          </AppButton>
        </template>
        <!-- aria-pressed: the same control opens and closes the action set,
             so it is a toggle, and its state must be audible as one. -->
        <AppButton
          icon-only
          size="sm"
          variant="ghost"
          :label="
            editing
              ? t('providers.account.doneEditing', { name: displayName })
              : t('providers.account.edit', { name: displayName })
          "
          :aria-pressed="editing"
          @click="editing = !editing"
        >
          <template #icon><ActionIcon :name="editing ? 'check' : 'edit'" /></template>
        </AppButton>
      </div>
    </div>

    <div v-if="windows.length" class="windows">
      <UsageBar v-for="(w, i) in windows" :key="i" :window="w" />
    </div>
    <p v-else-if="!usageEdgeBlocked" class="no-usage">
      {{ t("providers.account.noUsage") }}
    </p>

    <Banner v-if="account.error && !usageEdgeBlocked" tone="warn">
      {{ account.error }}
    </Banner>
  </div>
</template>

<style scoped>
.account {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
  padding: var(--space-4) 0;
}

/* Separator between rows rather than a border on every one, so the first row
   does not double up with the card header's own edge. */
.account + .account {
  border-top: 1px solid var(--border);
}

.account-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-3);
  flex-wrap: wrap;
}

.identity {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  min-width: 0;
  /* Grows past its content but may shrink to nothing before the actions wrap —
     a long email must ellipsize, not push Remove off the row. */
  flex: 1 1 200px;
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

.actions {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-shrink: 0;
}

.windows {
  display: grid;
  gap: var(--space-3);
}

.no-usage {
  margin: 0;
  color: var(--faint);
  font-size: var(--text-xs);
}

@media (max-width: 640px) {
  .actions {
    /* Full width below the identity: at 360px the two buttons and a long email
       cannot share a row without one of them being unreadable. */
    width: 100%;
  }

  /* The pencil stays square — only the labelled actions share the row. */
  .actions :deep(.btn:not(.btn-icon)) {
    flex: 1;
  }
}
</style>
