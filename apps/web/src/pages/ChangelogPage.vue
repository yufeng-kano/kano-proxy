<script setup lang="ts">
/**
 * Changelog — the one page in the app that is allowed to scroll.
 *
 * Everywhere else the anti-scroll rule puts long data in a bounded region with
 * a sticky header (docs/admin-ui.md § Anti-scroll rules). Release notes are the
 * exception: they are prose, read top to bottom, and paginating or
 * inner-scrolling them would fight the reading. Each release is one row of a
 * two-column timeline — a sticky version rail beside the notes — capped at
 * 960px rather than the full content width (docs/admin-ui.md § Changelog
 * page), and the page header stays sticky above it.
 *
 * `body_html` is sanitized twice server-side — GitHub's renderer, then the
 * Worker's escape-then-allowlist pass (docs/changelog.md § HTML sanitization) —
 * which is what makes `v-html` here correct rather than a hole.
 */
import { computed, onMounted } from "vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
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
    <PageHeader :title="t('changelog.title')">
      <template #actions>
        <!-- Icon-only: the label is a tooltip and the accessible name, so the
             control keeps its meaning without spending header width on a word
             that repeats on every page. -->
        <AppButton
          icon-only
          :label="t('action.refresh')"
          :loading="refreshing"
          @click="onRefresh"
        >
          <template #icon><ActionIcon name="refresh" /></template>
        </AppButton>
      </template>
    </PageHeader>

    <!-- Capped list, not full width: this is long-form reading, and a
         release note running the width of a 1440px display is unreadable. -->
    <div class="releases">
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

      <div v-if="showSkeleton" class="list">
        <span class="sr-only" role="status">{{ t("app.loading") }}</span>
        <div v-for="i in 3" :key="i" class="release" aria-hidden="true">
          <div class="rail">
            <span class="skeleton skeleton-tag" />
            <span class="skeleton skeleton-date" />
          </div>
          <div class="notes">
            <span class="skeleton skeleton-line" />
            <span class="skeleton skeleton-line" />
            <span class="skeleton skeleton-line short" />
          </div>
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

      <!-- A timeline: version rail on the left, notes on the right, a hairline
           between releases. No card per release — every release is the same
           dataset, so a border around each restated the one before it. -->
      <div v-else class="list">
        <article v-for="release in releases" :key="release.tag" class="release">
          <header class="rail">
            <div class="release-identity">
              <a
                class="release-tag"
                :href="release.url"
                target="_blank"
                rel="noopener noreferrer"
              >
                {{ release.tag }}
              </a>
              <!-- "Current" is enough beside the tag it sits on; read out of
                   context it is not, hence the spelled-out twin. -->
              <Badge v-if="isCurrent(release.tag)" tone="accent">
                <span aria-hidden="true">{{ t("changelog.currentShort") }}</span>
                <span class="sr-only">{{ t("changelog.current") }}</span>
              </Badge>
            </div>
            <time class="release-date" :datetime="release.published_at">
              {{ format.date(release.published_at) }}
            </time>
          </header>

          <div class="notes">
            <p v-if="releaseTitle(release)" class="release-name">
              {{ releaseTitle(release) }}
            </p>

            <!-- Sanitized server-side (GitHub's renderer, then ours) — see
                 docs/changelog.md. -->
            <!-- eslint-disable-next-line vue/no-v-html -->
            <div v-if="release.body_html" class="release-body" v-html="release.body_html" />
            <p v-else class="release-empty">{{ t("changelog.noNotes") }}</p>
          </div>
        </article>
      </div>
    </div>
  </div>
</template>

<style scoped>
/*
 * A capped list, not the page width. 960px is what the rail plus a wide
 * reading measure need (docs/admin-ui.md § Changelog page); the cap sits on
 * the list so the banners above it and every release line up on both edges.
 */
.releases {
  display: grid;
  gap: var(--space-4);
  max-width: 960px;
}

.list {
  display: grid;
}

/*
 * One release, one row: a fixed rail and the notes taking the rest. The
 * `minmax(0, 1fr)` matters — a bare `1fr` has a min-content floor, so one
 * unbroken token in the notes would widen the row past the cap.
 */
.release {
  display: grid;
  grid-template-columns: 176px minmax(0, 1fr);
  gap: var(--space-2) var(--space-8);
  padding: var(--space-6) 0;
  border-top: 1px solid var(--border);
}

/* The page header already draws the rule the first release would sit under.
   `first-of-type`, not `first-child`: the skeleton list leads with an sr-only
   status span. */
.release:first-of-type {
  border-top: 0;
  padding-top: var(--space-2);
}

/*
 * Sticky within its own release, under the sticky page header — so a long
 * release keeps its version in view while the notes scroll past. Grid
 * children stretch by default, and a stretched item has nowhere to stick
 * within; `align-self: start` is what makes the sticky work.
 *
 * The offset is the page header's stuck height: its top padding (the shell's
 * `--page-top`, inherited the same way PageHeader does), the 34px row floor
 * plus the row's bottom padding, and the hairline — then a breath.
 */
.rail {
  position: sticky;
  top: calc(var(--page-top, var(--space-6)) + 34px + var(--space-3) + 1px + var(--space-4));
  align-self: start;
  display: grid;
  gap: var(--space-1);
  min-width: 0;
}

.notes {
  min-width: 0;
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
  color: var(--faint);
  font-size: var(--text-xs);
  white-space: nowrap;
}

/* The release's one-line summary: the lead, so it reads in the text tone at
   the prose size rather than as a muted subtitle. */
.release-name {
  margin: 0 0 var(--space-3);
  color: var(--text);
  font-size: var(--text-base);
  font-weight: var(--weight-medium);
  line-height: 1.5;
}

.release-empty {
  margin: 0;
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
  color: var(--text-secondary);
  font-size: var(--text-base);
  line-height: 1.65;
  overflow-wrap: anywhere;
}

.release-body :deep(h2) {
  margin: var(--space-5) 0 var(--space-2);
  font-size: var(--text-md);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
  color: var(--text);
}

.release-body :deep(h3) {
  margin: var(--space-4) 0 var(--space-1);
  font-size: var(--text-base);
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
  font-size: var(--text-sm);
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

/* Shaped like a release row — a tag and date in the rail, three prose lines
   beside them — so the list does not jump when the notes land. Static, not
   pulsing. */
.skeleton {
  display: block;
  border-radius: var(--radius-full);
  background: var(--hover);
}

.skeleton-tag {
  width: 40%;
  height: var(--text-md);
}

.skeleton-date {
  width: 55%;
  height: var(--text-xs);
}

.skeleton-line {
  width: 100%;
  height: var(--text-sm);
  margin-bottom: var(--space-3);
}

.skeleton-line.short {
  width: 65%;
  margin-bottom: 0;
}

/* --- Responsive --------------------------------------------------------- */

/* One column: the rail becomes a row above the notes and stops sticking — a
   stuck block at the top of a phone viewport would cover the notes it names. */
@media (max-width: 720px) {
  .release {
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-3);
  }

  .rail {
    position: static;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: baseline;
    gap: var(--space-3);
  }

  .skeleton-tag {
    width: 96px;
  }

  .skeleton-date {
    width: 72px;
  }
}
</style>
