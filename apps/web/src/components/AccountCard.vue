<script setup lang="ts">
import { computed } from "vue"
import UsageBar from "@/components/UsageBar.vue"
import type { ProviderAccount } from "@/types"

const props = defineProps<{
  account: ProviderAccount
  busy?: boolean
}>()

const emit = defineEmits<{
  promote: []
  remove: []
}>()

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
  // Prefer email, then username/display, then stored label (should already be email after OAuth)
  if (email) return email
  if (username && username !== "claude" && username !== "codex" && username !== "grok") {
    return username
  }
  if (a.label && a.label !== "claude" && a.label !== "codex" && a.label !== "grok") {
    return a.label
  }
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
  const providerPrefixes = new Set(["default", "claude", "chatgpt", "codex", "grok", "subscription", "tier"])
  while (tokens.length > 1 && providerPrefixes.has(tokens[0]!)) tokens.shift()
  if (!tokens.length) return raw
  const special: Record<string, string> = { supergrok: "SuperGrok" }
  return tokens
    .map((t) => (/^\d+x$/.test(t) ? t : special[t] ?? t.charAt(0).toUpperCase() + t.slice(1)))
    .join(" ")
}

const secondaryLine = computed(() => {
  const meta = props.account.account ?? {}
  const plan =
    (typeof meta.plan_type === "string" && meta.plan_type) ||
    (typeof meta.rate_limit_tier === "string" && meta.rate_limit_tier) ||
    null
  return plan ? formatPlan(plan) : null
})

/** Codex usage API blocked by chatgpt.com edge — not a real account failure. */
function isUsageEdgeBlocked(error: string | null | undefined): boolean {
  return !!error && /edge blocked|403 bot challenge/i.test(error)
}
</script>

<template>
  <div class="account-row">
    <div class="account-top">
      <div class="account-meta">
        <span class="status-dot" :class="account.status" :title="account.status" />
        <span class="account-name">{{ displayName }}</span>
        <span v-if="secondaryLine" class="account-plan">{{ secondaryLine }}</span>
        <span class="status-pill">{{ account.status }}</span>
        <span
          v-if="account.stale && !isUsageEdgeBlocked(account.error)"
          class="status-pill"
          title="Usage may be stale"
        >stale</span>
      </div>
      <div class="account-actions">
        <button
          v-if="account.status !== 'active'"
          type="button"
          class="btn btn-secondary btn-sm"
          :disabled="busy"
          @click="emit('promote')"
        >
          Promote
        </button>
        <button
          type="button"
          class="btn btn-danger btn-sm"
          :disabled="busy"
          @click="emit('remove')"
        >
          Remove
        </button>
      </div>
    </div>

    <div v-if="account.usage?.windows?.length" class="usage-list">
      <UsageBar v-for="(w, i) in account.usage.windows" :key="i" :window="w" />
    </div>
    <!-- Codex usage edge bot-wall: omit empty-state and error (chat still works) -->
    <p
      v-else-if="!isUsageEdgeBlocked(account.error)"
      class="faint"
      style="margin: 0; font-size: 12.5px"
    >
      No usage data
    </p>

    <p
      v-if="account.error && !isUsageEdgeBlocked(account.error)"
      class="banner error"
      style="margin: 0"
    >
      {{ account.error }}
    </p>
  </div>
</template>
