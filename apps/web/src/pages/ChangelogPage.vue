<script setup lang="ts">
/**
 * Changelog — the one page in the app that is allowed to scroll.
 *
 * Everywhere else the anti-scroll rule puts long data in a bounded region with
 * a sticky header (docs/admin-ui.md § Anti-scroll rules). Release notes are the
 * exception: they are prose, read top to bottom, and paginating or
 * inner-scrolling them would fight the reading. So the column is capped at a
 * comfortable measure instead of the full content width, and the page header
 * stays sticky above it.
 *
 * `body_html` is sanitized twice server-side — GitHub's renderer, then the
 * Worker's escape-then-allowlist pass (docs/changelog.md § HTML sanitization) —
 * which is what makes `v-html` here correct rather than a hole.
 */
import { computed, onMounted } from "vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Badge from "@/components/ui/Badge.vue"
import Banner from "@/components/ui/Banner.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import PageHeader from "@/components/ui/PageHeader.vue"
import { useAuth } from "@/composables/useAuth"
import { useChangelog } from "@/composables/useChangelog"
import { useI18n } from "@/i18n"
import type { ChangelogRelease } from "@/types"

const { t, format } = useI18n()
const { user } = useAuth()
const { data, loading, refreshing, error, setUserId, load, refresh } = useChangelog()

const releases = computed<ChangelogRelease[]>(() => data.value?.releases ?? [])
const current = computed(() => data.value?.current ?? null)

/** Drops a leading `v` so a tag and the running version can be compared. */
function bare(version: string): string {
  return version.trim().replace(/^v/, "")
}

/**
 * Which card wears the "Current" badge. Tag and running version disagree on the
 * `v` prefix by design (`v1.11.0` vs `1.11.0`), hence the normalize. This is
 * *equality* for a badge, not an ordering — `updateAvailable` is the server's
 * call and is never recomputed here.
 */
function isCurrent(tag: string): boolean {
  const running = current.value
  return !!running && bare(tag) === bare(running)
}

/** Release names commonly repeat the tag ("v1.11.0 — …"); don't print it twice. */
function releaseTitle(release: ChangelogRelease): string | null {
  const name = release.name?.trim()
  if (!name || name === release.tag) return null
  const stripped = name.replace(/^v?\d+\.\d+\.\d+\s*[—–:-]?\s*/, "").trim()
  return stripped || null
}

const showSkeleton = computed(() => loading.value && !data.value)
const unavailable = computed(() => !!data.value && !data.value.available)
const empty = computed(
  () => !!data.value && data.value.available && releases.value.length === 0,
)

onMounted(() => {
  setUserId(user.value?.id ?? null)
  void load()
})

function onRefresh() {
  setUserId(user.value?.id ?? null)
  void refresh()
}
</script>

<template>
  <div>
    <PageHeader :title="t('changelog.title')" :subtitle="t('changelog.subtitle')">
      <template #actions>
        <AppButton :loading="refreshing" @click="onRefresh">
          {{ t("action.refresh") }}
        </AppButton>
      </template>
    </PageHeader>

    <!-- Capped measure, not full width: this is long-form reading, and a
         release note running the width of a 1440px display is unreadable. -->
    <div class="column">
      <Banner v-if="error" tone="error">
        {{ t("changelog.error.load") }}
        <template #actions>
          <AppButton size="sm" variant="ghost" @click="onRefresh">
            {{ t("action.retry") }}
          </AppButton>
        </template>
      </Banner>

      <Banner v-if="data?.updateAvailable && data.latest" tone="ok">
        {{ t("changelog.updateAvailable", { version: data.latest }) }}
      </Banner>

      <div v-if="showSkeleton" class="skeletons">
        <span class="sr-only" role="status">{{ t("app.loading") }}</span>
        <div v-for="i in 3" :key="i" class="skeleton-card" aria-hidden="true">
          <span class="skeleton skeleton-tag" />
          <span class="skeleton skeleton-line" />
          <span class="skeleton skeleton-line short" />
        </div>
      </div>

      <AppCard v-else-if="unavailable">
        <EmptyState
          :title="t('changelog.unavailable.title')"
          :body="t('changelog.unavailable.body')"
        />
      </AppCard>

      <AppCard v-else-if="empty">
        <EmptyState :title="t('changelog.empty.title')" :body="t('changelog.empty.body')" />
      </AppCard>

      <!-- `v-else` on a wrapper, not on the card: `v-if` and `v-for` on one
           element is ambiguous in Vue 3 and the compiler warns about it. -->
      <template v-else>
        <AppCard v-for="release in releases" :key="release.tag">
          <article>
            <header class="release-head">
              <div class="release-identity">
                <a
                  class="release-tag"
                  :href="release.url"
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  {{ release.tag }}
                </a>
                <Badge v-if="isCurrent(release.tag)" tone="accent">
                  {{ t("changelog.currentShort") }}
                </Badge>
              </div>
              <time class="release-date" :datetime="release.published_at">
                {{ format.date(release.published_at) }}
              </time>
            </header>

            <p v-if="releaseTitle(release)" class="release-name">
              {{ releaseTitle(release) }}
            </p>

            <!-- Sanitized server-side (GitHub's renderer, then ours) — see
                 docs/changelog.md. -->
            <!-- eslint-disable-next-line vue/no-v-html -->
            <div v-if="release.body_html" class="release-body" v-html="release.body_html" />
            <p v-else class="release-empty">{{ t("changelog.noNotes") }}</p>
          </article>
        </AppCard>
      </template>
    </div>
  </div>
</template>

<style scoped>
/* 72ch is the reading measure at --text-sm; the cap is on the column, not the
   cards, so every card lines up on both edges. */
.column {
  display: grid;
  gap: var(--space-4);
  max-width: 72ch;
}

.release-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--space-2) var(--space-3);
  flex-wrap: wrap;
}

.release-identity {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  min-width: 0;
}

.release-tag {
  font-size: var(--text-md);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
  color: var(--text);
}

.release-tag:hover {
  text-decoration: underline;
  text-underline-offset: 3px;
}

.release-date {
  flex-shrink: 0;
  color: var(--faint);
  font-size: var(--text-xs);
  white-space: nowrap;
}

.release-name {
  margin: var(--space-1) 0 0;
  color: var(--muted);
  font-size: var(--text-sm);
}

.release-empty {
  margin: var(--space-3) 0 0;
  color: var(--faint);
  font-size: var(--text-xs);
}

/*
 * `body_html` markup is injected, not written in this template, so scoped
 * attributes never land on it — every rule below needs :deep(). Heading sizes
 * stay under .release-tag: these sit *inside* a card and must not outrank the
 * release they belong to.
 */
.release-body {
  margin-top: var(--space-3);
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.65;
  overflow-wrap: anywhere;
}

.release-body :deep(h2) {
  margin: var(--space-5) 0 var(--space-2);
  font-size: var(--text-base);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
  color: var(--text);
}

.release-body :deep(h3) {
  margin: var(--space-4) 0 var(--space-1);
  font-size: var(--text-sm);
  font-weight: var(--weight-semibold);
  color: var(--text);
}

.release-body :deep(h2:first-child),
.release-body :deep(h3:first-child) {
  margin-top: 0;
}

.release-body :deep(p) {
  margin: 0 0 var(--space-3);
}

.release-body :deep(ul),
.release-body :deep(ol) {
  margin: 0 0 var(--space-3);
  padding-left: var(--space-5);
}

.release-body :deep(li) {
  margin: var(--space-1) 0;
}

.release-body :deep(li)::marker {
  color: var(--faint);
}

.release-body :deep(strong) {
  font-weight: var(--weight-semibold);
  color: var(--text);
}

.release-body :deep(em) {
  font-style: italic;
}

/*
 * `tt` is deprecated but GitHub still emits it inside compare links; without
 * this it falls back to the browser's default monospace at full size.
 */
.release-body :deep(code),
.release-body :deep(tt) {
  padding: 1px var(--space-1);
  border-radius: var(--radius-xs);
  background: var(--surface-2);
  border: 1px solid var(--border);
  font-family: var(--mono);
  font-size: var(--text-xs);
}

.release-body :deep(a) {
  color: var(--chart-input);
}

.release-body :deep(a:hover) {
  text-decoration: underline;
}

.release-body :deep(*:last-child) {
  margin-bottom: 0;
}

/* --- First paint -------------------------------------------------------- */

/* Shaped like a release card — a tag line over two prose lines — so the column
   does not jump when the notes land. Static, not pulsing. */
.skeletons {
  display: grid;
  gap: var(--space-4);
}

.skeleton-card {
  display: grid;
  gap: var(--space-3);
  padding: var(--space-5);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface);
}

.skeleton {
  display: block;
  border-radius: var(--radius-full);
  background: var(--hover);
}

.skeleton-tag {
  width: 30%;
  height: var(--text-md);
}

.skeleton-line {
  width: 100%;
  height: var(--text-sm);
}

.skeleton-line.short {
  width: 65%;
}
</style>
