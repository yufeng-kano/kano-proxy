<script setup lang="ts">
/**
 * Indeterminate progress, sized to the text it sits beside.
 *
 * Decorative by default: it always accompanies a label or an `aria-busy`
 * container that already announces the state, so a second announcement here
 * would be noise.
 */
withDefaults(defineProps<{ size?: number }>(), { size: 14 })
</script>

<template>
  <svg
    class="spinner"
    :width="size"
    :height="size"
    viewBox="0 0 16 16"
    fill="none"
    aria-hidden="true"
    focusable="false"
  >
    <circle cx="8" cy="8" r="6.5" stroke="currentColor" stroke-opacity="0.25" stroke-width="2" />
    <path
      d="M8 1.5A6.5 6.5 0 0 1 14.5 8"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
    />
  </svg>
</template>

<style scoped>
.spinner {
  animation: spin 700ms linear infinite;
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

/* Reduced motion still needs *some* signal that work is in flight, so the
   spin becomes a slow pulse rather than disappearing entirely. */
@media (prefers-reduced-motion: reduce) {
  .spinner {
    animation: pulse 1.4s ease-in-out infinite;
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.4;
    }
  }
}
</style>
