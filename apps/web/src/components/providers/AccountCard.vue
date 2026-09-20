<script setup lang="ts">
/**
 * One connected subscription account inside a provider section.
 *
 * A row rather than a card-in-a-card: the section's AppCard is already the
 * surface, so this only needs an identity line, its usage windows, and the
 * actions. Those are gated by `editing`, which the *section* owns (docs/admin-ui.md
 * § Providers page) — one toggle in the card header opens every row at once,
 * rather than each row carrying a pencil that restates the same affordance. A
 * resting row is identity and usage only, so the trash is never one accidental
 * click away. The status dot always ships its text label (§ Accessibility floor).
 */
import { computed } from "vue"
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
  /**
   * First in the pool's priority order — where Move up ends.
   * Deliberately not `status === 'active'`: a limited or paused primary is
   * still the primary, and that is exactly when the operator needs to see that
   * the pool's first choice is not where traffic is going (docs/admin-ui.md
   * § Providers page).
   */
  primary?: boolean
  busy?: boolean
  /** The section's gate — the row's actions render only while this is on. */
  editing?: boolean
  /** The pool has 2+ rows; with one there is no order to change. */
  reorderable?: boolean
  canMoveUp?: boolean
  canMoveDown?: boolean
  /** Only read to explain a permanently empty usage area; identity is provider-agnostic. */
  provider?: string
}>()

const emit = defineEmits<{
  resume: []
  moveUp: []
  moveDown: []
  /** Carries the upstream identity: the dialog shows what a blank name falls back to. */
  rename: [identity: string]
  remove: []
}>()

const { t } = useI18n()

/**
 * Server data, not copy: an email or upstream username identifies the account
 * to its owner. The generic provider names are rejected as identities because
 * upstream returns them as a placeholder for "no profile", where the account
 * id at least distinguishes two accounts from each other.
 */
const GENERIC_LABELS = new Set(["claude", "codex", "grok", "antigravity"])

/**
 * The real account behind the row — the email/username upstream reports.
 * `label` is only a candidate here while it is not the custom name: the API
 * resolves that field to the custom one when set, and repeating it under the
 * title would print the same string twice.
 */
const identity = computed(() => {
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
  const label = a.label && a.label !== a.custom_label ? a.label : null
  if (email) return email
  if (username && !GENERIC_LABELS.has(username)) return username
  if (label && !GENERIC_LABELS.has(label)) return label
  return email || username || label || a.id.slice(0, 12)
})

/** A user's own name wins the title; the identity below keeps it traceable. */
const displayName = computed(() => props.account.custom_label || identity.value)

const renamed = computed(() => !!props.account.custom_label)

/**
 * A row the viewer borrows from another user rather than owns
 * (docs/admin-ui.md § Providers page). It arrives with no usage surface at
 * all — no windows, no probe error, no upstream identity blob — and the only
 * mutation a borrower may make on it is Move up / down, which reorders it
 * inside their own pool; Rename, Resume and Remove are the owner's alone and
 * answer 403, so they are not rendered here.
 */
const share = computed(() => props.account.share ?? null)

const sharedBy = computed(() =>
  share.value ? t("providers.account.shared", { owner: share.value.ownerLabel }) : null,
)

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
    "antigravity",
    "subscription",
    "tier",
  ])
  while (tokens.length > 1 && providerPrefixes.has(tokens[0]!)) tokens.shift()
  if (!tokens.length) return raw
  const special: Record<string, string> = { supergrok: "SuperGrok", ai: "AI" }
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
 * Usage-probe failures the operator cannot act on (docs/admin-ui.md
 * § Providers page). Codex's usage API sits behind chatgpt.com's bot wall
 * (chat still routes); Anthropic's usage endpoint can return `usage 429`
 * on its own budget while Messages traffic is fine. The last good windows
 * stay on the bars — a banner restating the probe error is noise. Token /
 * auth failures still surface.
 */
const hideUsageError = computed(() => {
  const error = props.account.error
  if (!error) return false
  return /edge blocked|403 bot challenge/i.test(error) || /^\s*usage 429\s*$/i.test(error)
})

const windows = computed(() => props.account.usage?.windows ?? [])

/**
 * Antigravity's Google One AI credit balance — a real upstream number with no
 * total and no reset, so it can never be a bar (docs/providers.md
 * § Antigravity). It rides alongside the quota bars as a plain line, and is
 * frequently absent: Google often sends the usage floor without the balance.
 */
const creditsText = computed(() => {
  if (props.provider !== "antigravity") return null
  const credits = props.account.account?.credits_remaining
  return typeof credits === "number"
    ? t("providers.account.credits", { credits: credits.toLocaleString() })
    : null
})
</script>

<template>
  <div class="account">
    <div class="account-head">
      <div class="identity">
        <span class="name" :title="displayName">{{ displayName }}</span>
        <!-- A renamed row still has to answer "which account is this?". -->
        <span v-if="renamed && !share" class="upstream" :title="identity">{{ identity }}</span>
        <div class="tags">
          <StatusDot :status="account.status" />
          <!-- The badge marks the top of the order the move arrows edit. The
               dot beside it answers the other question — where requests go
               right now. -->
          <Badge v-if="primary" tone="accent">
            {{ t("providers.account.primary") }}
          </Badge>
          <Badge v-if="plan">{{ t("providers.account.plan", { plan }) }}</Badge>
          <!-- The one fact on this row the viewer does not own: who lends it. The
               name can be long, so the pill caps its width and keeps the whole
               string in its title rather than clipping mid-word. -->
          <Badge v-if="sharedBy" class="share" :title="sharedBy">
            <span class="share-owner">{{ sharedBy }}</span>
          </Badge>
        </div>
      </div>

      <!-- The blank space at the row's right edge, filled only while the
           section's gate is open. All icons (docs/admin-ui.md § Providers
           page): each glyph's words live in `label`, which is both the
           accessible name and the tooltip, and each name carries the account —
           several rows offer the same actions. Remove is the trash in the
           danger tone, last in the row, and confirms before it deletes. -->
      <div v-if="editing" class="actions">
        <template v-if="reorderable">
          <AppButton
            icon-only
            size="sm"
            variant="ghost"
            :label="t('providers.account.moveUp', { name: displayName })"
            :disabled="busy || !canMoveUp"
            @click="emit('moveUp')"
          >
            <template #icon><ActionIcon name="arrow-up" /></template>
          </AppButton>
          <AppButton
            icon-only
            size="sm"
            variant="ghost"
            :label="t('providers.account.moveDown', { name: displayName })"
            :disabled="busy || !canMoveDown"
            @click="emit('moveDown')"
          >
            <template #icon><ActionIcon name="arrow-down" /></template>
          </AppButton>
        </template>
        <AppButton
          v-if="account.status === 'benched' && !share"
          icon-only
          size="sm"
          variant="ghost"
          :label="t('providers.account.resume', { name: displayName })"
          :disabled="busy"
          @click="emit('resume')"
        >
          <template #icon><ActionIcon name="play" /></template>
        </AppButton>
        <AppButton
          v-if="!share"
          icon-only
          size="sm"
          variant="ghost"
          :label="t('providers.account.rename', { name: displayName })"
          :disabled="busy"
          @click="emit('rename', identity)"
        >
          <template #icon><ActionIcon name="edit" /></template>
        </AppButton>
        <AppButton
          v-if="!share"
          icon-only
          size="sm"
          variant="danger"
          :label="t('providers.account.remove', { name: displayName })"
          :disabled="busy"
          @click="emit('remove')"
        >
          <template #icon><ActionIcon name="trash" /></template>
        </AppButton>
      </div>
    </div>

    <!-- A shared row never reads the owner's usage surface itself — no probe
         error, and nothing here ever asks upstream about someone else's
         account. What it shows are the bars the edition hands it: the
         borrower's own allowance on this row, plus the account's stored
         windows where its owner chose to show them. Without any, the usage
         area is simply absent. -->
    <div v-if="share && windows.length" class="windows">
      <UsageBar v-for="(w, i) in windows" :key="i" :window="w" />
    </div>
    <template v-else-if="!share">
      <div v-if="windows.length" class="windows">
        <UsageBar v-for="(w, i) in windows" :key="i" :window="w" />
      </div>
      <!-- The balance sits under the bars when both exist, and stands in for
           them when the quota read came back empty. -->
      <p v-if="creditsText" class="no-usage">
        {{ creditsText }}
      </p>
      <p v-else-if="!windows.length && !hideUsageError" class="no-usage">
        {{ t("providers.account.noUsage") }}
      </p>

      <Banner v-if="account.error && !hideUsageError" tone="warn">
        {{ account.error }}
      </Banner>
    </template>
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
     a long email must ellipsize, not push the trash off the row. The basis is
     the wrap threshold, not spacing: it has to clear five icon buttons plus the
     gutters at 360px, or the flex line breaks and the actions drop below. */
  flex: 1 1 120px;
}

.name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
}

.upstream {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--muted);
  font-size: var(--text-xs);
}

.tags {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
}

/* An owner name is open-ended text in a track that can get narrow, and a pill
   cannot wrap: cap it and ellipsize inside, with the full string in the title. */
.share {
  max-width: min(100%, 16rem);
  overflow: hidden;
}

/* min-width: 0 so the flex item may shrink below its text — without it the
   name pushes past the cap instead of ellipsizing. */
.share-owner {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* Stays on the identity row at every width, right-aligned — the pencil sits
   vertically under the section's Add control, so a section has one column of
   controls down its right edge (docs/admin-ui.md § Providers page). */
.actions {
  display: flex;
  align-items: center;
  gap: var(--space-1);
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
</style>
