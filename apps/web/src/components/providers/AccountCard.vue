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
 * click away — except Sign in again on an expired sign-in, which is the fix for
 * what the dot reports and destroys nothing. The status dot always ships its
 * text label (§ Accessibility floor).
 */
import { computed, ref, watch } from "vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import Badge from "@/components/ui/Badge.vue"
import StatusDot from "@/components/ui/StatusDot.vue"
import UsageBar from "@/components/ui/UsageBar.vue"
import { useI18n } from "@/i18n"
import type { CodexCredits, ProviderAccount } from "@/types"

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
  /** Carries the display name: the sign-in dialog says which account to use. */
  reconnect: [name: string]
  /** The owner's new choice for routing on credits after the plan's limits. */
  allowCredits: [allow: boolean]
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
 * (docs/admin-ui.md § Providers page). It arrives with no probe error and no
 * upstream identity blob; its usage is whatever the edition hands the
 * borrower (their allowance, and where the edition shows them, the account's
 * windows and Codex balance). The only
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
 * stay on the bars — a note restating the probe error is noise. Auth
 * failures surface as the `reauth` status instead.
 */
const hideUsageError = computed(() => {
  const error = props.account.error
  if (!error) return false
  return /edge blocked|403 bot challenge/i.test(error) || /^\s*usage 429\s*$/i.test(error)
})

const windows = computed(() => props.account.usage?.windows ?? [])

/**
 * The viewer's own row whose sign-in expired (docs/admin-ui.md § Providers
 * page). The status and the Sign in again button say it; the raw probe error
 * ("usage 401") is not printed, and the bars are last-known values.
 */
const reauth = computed(() => props.account.status === "reauth" && !share.value)

/**
 * Any other probe failure worth seeing: a short note on the tags line, not a
 * banner. The last good windows stay on the bars; the upstream text is data,
 * so it rides in the tooltip and the accessible name rather than as copy.
 */
const usageError = computed(() =>
  !share.value && props.account.status !== "reauth" && !hideUsageError.value
    ? props.account.error
    : null,
)

/**
 * Codex's credit balance, when upstream reported one: in the identity blob on
 * an own row, and on a shared row, which has none, only where the edition
 * handed it with the account's windows (docs/admin-ui.md § Providers page).
 */
const codexCredits = computed<CodexCredits | null>(() => {
  if (props.provider !== "codex") return null
  const credits = share.value ? props.account.usage?.credits : props.account.account?.credits
  return credits && typeof credits === "object" ? (credits as CodexCredits) : null
})

/**
 * A credit balance — a real upstream number with no total and no reset, so it
 * can never be a bar (docs/admin-ui.md § Providers page). It is the amount of
 * a Credits row under the bars, and is absent whenever upstream sent none:
 * Antigravity's Google One AI balance often goes missing, and a Codex account
 * without credits gets no row rather than an invented zero. Credits without a
 * balance read "Available", as the Codex CLI prints them: routing counts them
 * as usable, so the row must not go silent beside an On credits badge. A Codex
 * balance that does not parse is upstream's text verbatim.
 */
const creditsText = computed(() => {
  if (props.provider === "antigravity") {
    const credits = props.account.account?.credits_remaining
    return typeof credits === "number"
      ? t("providers.account.creditsLeft", { credits: creditAmount(credits) })
      : null
  }
  const codex = codexCredits.value
  if (!codex) return null
  if (codex.unlimited) return t("providers.account.creditsUnlimited")
  if (!codex.has_credits) return null
  if (typeof codex.balance !== "string") return t("providers.account.creditsAvailable")
  const n = Number(codex.balance)
  const parses = codex.balance.trim() !== "" && Number.isFinite(n)
  return t("providers.account.creditsLeft", { credits: parses ? creditAmount(n) : codex.balance })
})

/**
 * Whole credits from one credit up, as the Codex CLI rounds them: upstream
 * sends "12345.678", and the fraction is noise beside a balance that size.
 * Under one credit the fraction is the whole balance, so it stays — rounding
 * would print 0 on a row that still routes on it.
 */
function creditAmount(n: number): number {
  return n >= 1 ? Math.round(n) : n
}

/**
 * The opt-in to keep routing on credits after the plan's limits: the owner's
 * alone (a shared row answers 403), and only where there are credits to spend.
 * An opted-in row keeps the control after its balance runs out, so the choice
 * can always be undone.
 */
const showAllowCredits = computed(
  () =>
    !!props.editing &&
    !share.value &&
    props.provider === "codex" &&
    (!!codexCredits.value?.has_credits ||
      !!codexCredits.value?.unlimited ||
      props.account.allow_credits),
)

/**
 * The native checkbox flips itself on click, and Vue patches nothing back
 * when the bound value never changed — so once the write settles, the box is
 * put back to whatever the reloaded row says (a failure reverts it).
 */
const allowCreditsBox = ref<HTMLInputElement | null>(null)
watch(
  () => props.busy,
  (busy) => {
    if (!busy && allowCreditsBox.value) {
      allowCreditsBox.value.checked = props.account.allow_credits
    }
  },
)
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
          <!-- The dot says the row is routable; this says what that costs. -->
          <Badge v-if="account.on_credits" tone="warn">
            {{ t("providers.account.onCredits") }}
          </Badge>
          <!-- Outside the section's gate on purpose: it is the fix for the
               state the dot beside it reports, and it destroys nothing. -->
          <AppButton
            v-if="reauth"
            class="reconnect"
            size="sm"
            variant="ghost"
            :label="t('providers.account.reconnectLabel', { name: displayName })"
            :disabled="busy"
            @click="emit('reconnect', displayName)"
          >
            {{ t("providers.account.reconnect") }}
          </AppButton>
          <span v-if="usageError" class="usage-error" :title="usageError">
            <span aria-hidden="true">{{ t("providers.account.usageStale") }}</span>
            <span class="sr-only">
              {{ t("providers.account.usageStaleDetail", { error: usageError }) }}
            </span>
          </span>
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
         account. What it shows is what the edition hands it: the borrower's
         own allowance on this row, plus the account's stored windows and
         Codex balance where its owner chose to show them. Without any, the
         usage area is simply absent. An expired sign-in, which only an own
         row reports, freezes the bars and the balance at their last known
         values, so they are muted rather than read as live. -->
    <div v-if="windows.length || creditsText" class="windows" :class="{ frozen: reauth }">
      <UsageBar v-for="(w, i) in windows" :key="i" :window="w" />
      <!-- The balance sits under the bars when both exist, and stands in for
           them when the quota read came back empty. -->
      <div v-if="creditsText" class="credits">
        <span class="credits-label">{{ t("providers.account.credits") }}</span>
        <span class="credits-value tabular">{{ creditsText }}</span>
      </div>
    </div>
    <!-- Usage never "appears" on a shared row or on an expired sign-in, so
         neither promises it. -->
    <p v-else-if="!share && !hideUsageError && !reauth" class="no-usage">
      {{ t("providers.account.noUsage") }}
    </p>
    <!-- Behind the section's gate: it changes routing and spends money, so
         it sits with the pool's other edits rather than on the resting row. -->
    <label v-if="showAllowCredits" class="check">
      <input
        ref="allowCreditsBox"
        type="checkbox"
        :checked="account.allow_credits"
        :disabled="busy"
        @change="emit('allowCredits', ($event.target as HTMLInputElement).checked)"
      />
      <span class="check-text">
        <span class="check-title">{{ t("providers.account.allowCredits") }}</span>
        <span class="check-hint">{{ t("providers.account.allowCreditsHint") }}</span>
      </span>
    </label>
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

/* A text button, not a 28px box: it sits in a line of 20px pills and must not
   grow the row. The underline is what says it can be pressed. */
.tags .reconnect {
  height: auto;
  padding: 0;
  color: var(--text);
  text-decoration: underline;
  text-underline-offset: 2px;
}

.tags .reconnect:hover {
  background: transparent;
  text-decoration-thickness: 2px;
}

.usage-error {
  color: var(--warn);
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
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

/* Label, track and amount are columns of the whole list, not of each bar:
   UsageBar and the credits row take them as a subgrid, so every track ends at
   the same x and the amounts line up however wide one is — an edition's
   "$2.5 / $10" beside a plain "100%". The label column is sized for the
   longest label any adapter emits — Antigravity's group-prefixed
   "Gemini Week" (docs/providers.md § Antigravity) — not for a bare "5h". */
.windows {
  display: grid;
  grid-template-columns: 78px minmax(0, 1fr) minmax(44px, auto);
  gap: var(--space-3);
}

.windows.frozen {
  opacity: 0.55;
}

/* A balance has no total, so no track: its amount spans the empty track
   column, right-aligned with the bars' amounts, and so never widens their
   column. Set like a bar's label and amount — a value, not a footnote. */
.credits {
  display: grid;
  grid-column: 1 / -1;
  grid-template-columns: subgrid;
  align-items: center;
}

.credits-label {
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
  color: var(--text-secondary);
}

.credits-value {
  grid-column: 2 / -1;
  font-size: var(--text-xs);
  color: var(--text-secondary);
  text-align: right;
}

/* The amount column grows to its widest amount rather than wrapping it. */
@media (max-width: 640px) {
  .windows {
    grid-template-columns: 68px minmax(0, 1fr) minmax(40px, auto);
    column-gap: var(--space-2);
  }
}

.no-usage {
  margin: 0;
  color: var(--faint);
  font-size: var(--text-xs);
}

/* Same checkbox spec as the key dialog's: box, then label over its hint. */
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
  color: var(--muted);
  font-size: var(--text-xs);
}
</style>
