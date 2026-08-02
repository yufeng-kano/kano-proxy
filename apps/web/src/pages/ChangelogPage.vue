<script setup lang="ts">
import { computed, onMounted } from "vue"
import { useAuth } from "@/composables/useAuth"
import { useChangelog } from "@/composables/useChangelog"
import type { ChangelogRelease } from "@/types"

const { user } = useAuth()
const { data, loading, refreshing, error, fromCache, setUserId, load, refresh } =
  useChangelog()

const releases = computed<ChangelogRelease[]>(() => data.value?.releases ?? [])
const current = computed(() => data.value?.current ?? null)

/** Drops a leading `v` so a tag and the running version can be compared. */
function bare(version: string): string {
  return version.trim().replace(/^v/, "")
}

/**
 * Which card wears the "Current" pill. Tag and running version disagree on the
 * `v` prefix by design (`v1.11.0` vs `1.11.0`), hence the normalize. This is
 * *equality* for a badge, not an ordering — `updateAvailable` is the server's
 * call and is never recomputed here.
 */
function isCurrent(tag: string): boolean {
  const running = current.value
  return !!running && bare(tag) === bare(running)
}

function formatDate(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  })
}

/** Release names commonly repeat the tag ("v1.11.0 — …"); don't print it twice. */
function releaseTitle(release: ChangelogRelease): string | null {
  const name = release.name?.trim()
  if (!name || name === release.tag) return null
  const stripped = name.replace(/^v?\d+\.\d+\.\d+\s*[—–:-]?\s*/, "").trim()
  return stripped || null
}

onMounted(() => {
  setUserId(user.value?.id ?? null)
  void load()
})

function onManualRefresh() {
  setUserId(user.value?.id ?? null)
  void refresh()
}
</script>

<template>
  <div>
    <div class="page-header">
      <div>
        <h1 class="page-title">Changelog</h1>
        <p class="page-sub">
          Published releases, newest first.
          <span v-if="current">Running <code class="mono">v{{ current }}</code>.</span>
          <span v-if="fromCache" class="faint"> · showing cache</span>
          <span v-if="refreshing" class="faint"> · refreshing…</span>
        </p>
      </div>
      <button
        type="button"
        class="btn btn-secondary"
        :disabled="loading || refreshing"
        @click="onManualRefresh"
      >
        Refresh
      </button>
    </div>

    <p v-if="error" class="banner error">{{ error }}</p>
    <p v-if="loading" class="muted">Loading…</p>

    <template v-else-if="data">
      <p v-if="data.updateAvailable && data.latest" class="banner" style="margin-bottom: 16px">
        A newer release is available: <strong>{{ data.latest }}</strong>.
      </p>
      <p v-if="data.stale" class="faint stale-hint">
        Could not reach GitHub just now — showing the last release notes we have.
      </p>

      <p v-if="!data.available" class="empty card card-pad">
        Release notes are unavailable for this deployment.
        <span v-if="data.error" class="faint"> {{ data.error }}</span>
      </p>
      <p v-else-if="!releases.length" class="empty card card-pad">
        No published releases yet.
      </p>

      <div v-else class="section-grid">
        <article v-for="release in releases" :key="release.tag" class="card release-card">
          <header class="release-head">
            <div class="release-title">
              <a :href="release.url" target="_blank" rel="noopener noreferrer" class="release-tag">
                {{ release.tag }}
              </a>
              <span v-if="isCurrent(release.tag)" class="status-pill">Current</span>
            </div>
            <time class="release-date faint" :datetime="release.published_at">
              {{ formatDate(release.published_at) }}
            </time>
          </header>
          <p v-if="releaseTitle(release)" class="release-name">{{ releaseTitle(release) }}</p>

          <!-- Sanitized server-side (GitHub's renderer, then ours) — see docs/changelog.md. -->
          <!-- eslint-disable-next-line vue/no-v-html -->
          <div v-if="release.body_html" class="release-body" v-html="release.body_html" />
          <p v-else class="faint" style="margin: 0; font-size: 12.5px">No release notes.</p>
        </article>
      </div>
    </template>
  </div>
</template>

<style scoped>
.stale-hint {
  margin: 0 0 12px;
  font-size: 12.5px;
}

.release-card {
  padding: 16px 18px;
}

.release-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 8px 12px;
}

.release-title {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.release-tag {
  font-size: 16px;
  font-weight: 600;
  letter-spacing: -0.02em;
  color: var(--text);
}

.release-tag:hover {
  text-decoration: underline;
}

.release-date {
  font-size: 12.5px;
  white-space: nowrap;
}

.release-name {
  margin: 4px 0 0;
  color: var(--muted);
  font-size: 13px;
}

/*
 * `body_html` markup is injected, not written in this template, so scoped
 * attributes never land on it — every rule below needs :deep(). Sizes stay
 * under .release-tag (16px): these headings sit *inside* a card and must not
 * outrank its own title.
 */
.release-body {
  margin-top: 12px;
  color: var(--text-secondary);
  font-size: 13.5px;
  line-height: 1.6;
  overflow-wrap: anywhere;
}

.release-body :deep(h2) {
  margin: 18px 0 6px;
  font-size: 14px;
  font-weight: 650;
  letter-spacing: -0.01em;
  color: var(--text);
}

.release-body :deep(h3) {
  margin: 14px 0 4px;
  font-size: 13px;
  font-weight: 600;
  color: var(--text);
}

.release-body :deep(h2:first-child),
.release-body :deep(h3:first-child) {
  margin-top: 0;
}

.release-body :deep(p) {
  margin: 0 0 10px;
}

.release-body :deep(ul),
.release-body :deep(ol) {
  margin: 0 0 10px;
  padding-left: 20px;
}

.release-body :deep(li) {
  margin: 3px 0;
}

.release-body :deep(li)::marker {
  color: var(--faint);
}

.release-body :deep(strong) {
  font-weight: 600;
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
  font-family: var(--mono);
  font-size: 12px;
  padding: 1px 5px;
  border-radius: var(--radius-sm);
  background: var(--surface-2);
  border: 1px solid var(--border);
}

.release-body :deep(a) {
  color: var(--chart-input);
  text-decoration: none;
}

.release-body :deep(a:hover) {
  text-decoration: underline;
}

.release-body :deep(*:last-child) {
  margin-bottom: 0;
}
</style>
