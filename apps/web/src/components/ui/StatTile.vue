<script setup lang="ts">
/**
 * One headline number.
 *
 * `hero` is the metric the page is *about* — larger, spanning two columns, and
 * carrying a meter. At most one per page: a grid where everything is
 * emphasized emphasizes nothing.
 */
defineProps<{
  label: string
  value: string
  /** Coverage or context under the value — never a mechanism note. */
  note?: string | null
  hero?: boolean
  /** 0–100 meter fill, hero only. Null hides the meter. */
  meter?: number | null
  loading?: boolean
}>()
</script>

<template>
  <div class="tile" :class="{ hero }">
    <span class="tile-label">{{ label }}</span>
    <span v-if="loading" class="tile-skeleton" aria-hidden="true" />
    <span v-else class="tile-value tabular">{{ value }}</span>

    <div v-if="hero && meter != null" class="meter">
      <div class="meter-fill" :style="{ width: `${meter}%` }" />
    </div>
    <span v-if="note" class="tile-note">{{ note }}</span>
  </div>
</template>

<style scoped>
.tile {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  min-width: 0;
  padding: var(--space-4);
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow);
}

.tile-label {
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
  color: var(--muted);
}

.tile-value {
  font-size: var(--text-xl);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tighter);
  line-height: 1.15;
}

.hero .tile-value {
  font-size: var(--text-2xl);
}

.tile-note {
  margin-top: auto;
  padding-top: var(--space-1);
  font-size: var(--text-2xs);
  color: var(--faint);
  line-height: 1.4;
}

.tile-skeleton {
  width: 60%;
  height: calc(var(--text-xl) * 1.15);
  border-radius: var(--radius-xs);
  background: var(--surface-2);
  animation: shimmer 1.4s var(--ease) infinite;
}

.hero .tile-skeleton {
  height: calc(var(--text-2xl) * 1.15);
}

@keyframes shimmer {
  50% {
    opacity: 0.5;
  }
}

/* Cache rate reads as neutral magnitude, not severity — deliberately not the
   usage bar's amber/red escalation: a higher number here is not "worse". */
.meter {
  height: 5px;
  margin-top: var(--space-2);
  border-radius: var(--radius-full);
  background: var(--bar-track);
  overflow: hidden;
}

.meter-fill {
  height: 100%;
  border-radius: var(--radius-full);
  background: var(--chart-input);
  transition: width var(--duration-slow) var(--ease);
}
</style>
