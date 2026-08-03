<script setup lang="ts">
/** Small non-interactive label: a format tag, a count, a "Current" marker. */
withDefaults(
  defineProps<{
    tone?: "neutral" | "accent" | "ok" | "warn" | "danger"
    mono?: boolean
  }>(),
  { tone: "neutral" },
)
</script>

<template>
  <span class="badge" :class="[tone, { mono }]"><slot /></span>
</template>

<style scoped>
.badge {
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
  flex-shrink: 0;
  padding: 2px var(--space-2);
  border-radius: var(--radius-full);
  font-size: var(--text-2xs);
  font-weight: var(--weight-medium);
  line-height: 1.5;
  white-space: nowrap;
  border: 1px solid transparent;
}

.neutral {
  background: var(--hover);
  color: var(--muted);
}

.accent {
  background: var(--accent);
  color: var(--accent-fg);
}

.ok {
  background: var(--ok-bg);
  border-color: var(--ok-border);
  color: var(--ok);
}

.warn {
  background: var(--warn-bg);
  border-color: var(--warn-border);
  color: var(--warn);
}

.danger {
  background: var(--danger-bg);
  border-color: var(--danger-border);
  color: var(--danger);
}
</style>
