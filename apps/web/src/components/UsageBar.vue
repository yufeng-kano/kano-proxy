<script setup lang="ts">
import { computed } from "vue"
import type { UsageWindow } from "@/types"

const props = defineProps<{ window: UsageWindow }>()

const pct = computed(() => {
  const u = props.window.utilization
  if (u == null || Number.isNaN(u)) return null
  // API may send 0–1 or 0–100
  const n = u <= 1 ? u * 100 : u
  return Math.max(0, Math.min(100, Math.round(n)))
})

const fillClass = computed(() => {
  if (pct.value == null) return ""
  if (pct.value >= 90) return "high"
  if (pct.value >= 70) return "mid"
  return ""
})

const resetText = computed(() => {
  const r = props.window.resets_at
  if (!r) return null
  try {
    const d = new Date(r)
    if (Number.isNaN(d.getTime())) return r
    return `resets ${d.toLocaleString()}`
  } catch {
    return r
  }
})
</script>

<template>
  <div class="usage-row">
    <span class="usage-label">{{ window.label }}</span>
    <div class="usage-track" :title="pct == null ? 'n/a' : `${pct}%`">
      <div
        class="usage-fill"
        :class="fillClass"
        :style="{ width: pct == null ? '0%' : `${pct}%` }"
      />
    </div>
    <span class="usage-pct">{{ pct == null ? "—" : `${pct}%` }}</span>
    <span v-if="resetText" class="usage-reset">{{ resetText }}</span>
  </div>
</template>
