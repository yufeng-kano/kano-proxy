<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { listModels } from "@/services/api"
import {
  CACHE_TTL_MS,
  isModelsCacheFresh,
  readModelsCache,
  writeModelsCache,
} from "@/services/cache"
import { useAuth } from "@/composables/useAuth"
import { useCustomProviders } from "@/composables/useCustomProviders"
import {
  PROVIDERS,
  type CatalogModel,
  type ModelsResponse,
} from "@/types"

type ModelGroup = {
  key: string
  name: string
  /** Subtitle line under the title — builtin blurb, or `slug/*` for custom providers. */
  blurb: string | null
  formatBadge: string | null
  models: CatalogModel[]
  error: string | null
  emptyKind: "codex" | "custom" | "generic"
}

const { user } = useAuth()
const customProviders = useCustomProviders()
const models = ref<CatalogModel[]>([])
const providerMeta = ref<Array<{ provider: string; count: number; error: string | null; cached: boolean }>>([])
const loading = ref(true)
const error = ref<string | null>(null)
const copied = ref<string | null>(null)
const fromCache = ref(false)

/** Split on the first "/" only — an upstream id may itself contain further "/". */
function prefixOf(id: string): string {
  const i = id.indexOf("/")
  return i === -1 ? id : id.slice(0, i)
}

const grouped = computed<ModelGroup[]>(() => {
  const byPrefix = new Map<string, CatalogModel[]>()
  for (const m of models.value) {
    const key = prefixOf(m.id)
    const list = byPrefix.get(key)
    if (list) list.push(m)
    else byPrefix.set(key, [m])
  }
  const metaFor = (key: string) => providerMeta.value.find((x) => x.provider === key)

  const groups: ModelGroup[] = []
  const seen = new Set<string>()

  // 1. The 3 builtin providers — fixed order, unchanged metadata/empty-states.
  for (const p of PROVIDERS) {
    seen.add(p.id)
    groups.push({
      key: p.id,
      name: p.name,
      blurb: p.blurb,
      formatBadge: null,
      models: byPrefix.get(p.id) ?? [],
      error: metaFor(p.id)?.error ?? null,
      emptyKind: p.id === "codex" ? "codex" : "generic",
    })
  }

  // 2. One group per custom provider the user has defined, in API order.
  for (const cp of customProviders.state.data ?? []) {
    seen.add(cp.slug)
    groups.push({
      key: cp.slug,
      name: cp.name,
      blurb: `${cp.slug}/*`,
      formatBadge: cp.format === "anthropic" ? "Anthropic" : "OpenAI",
      models: byPrefix.get(cp.slug) ?? [],
      error: metaFor(cp.slug)?.error ?? null,
      emptyKind: "custom",
    })
  }

  // 3. Defensive: any response prefix matching neither a builtin nor a known
  // custom provider (e.g. stale cache right after a provider was deleted)
  // still renders instead of silently dropping models.
  for (const [key, list] of byPrefix) {
    if (seen.has(key)) continue
    groups.push({
      key,
      name: key,
      blurb: null,
      formatBadge: null,
      models: list,
      error: metaFor(key)?.error ?? null,
      emptyKind: "generic",
    })
  }

  return groups
})

async function load(opts?: { force?: boolean }) {
  const uid = user.value?.id ?? null
  error.value = null
  customProviders.setUserId(uid)

  if (!opts?.force) {
    const cached = readModelsCache(uid)
    if (cached) {
      applyResponse(cached)
      fromCache.value = true
      loading.value = false
      if (isModelsCacheFresh(uid, CACHE_TTL_MS)) {
        void customProviders.load()
        return
      }
    }
  }

  if (!models.value.length) loading.value = true
  try {
    const [res] = await Promise.all([
      listModels({ refresh: !!opts?.force }),
      customProviders.load({ refresh: !!opts?.force }),
    ])
    applyResponse(res)
    fromCache.value = false
    writeModelsCache(uid, res)
  } catch (e) {
    error.value = e instanceof Error ? e.message : "Failed to load models"
  } finally {
    loading.value = false
  }
}

function applyResponse(res: ModelsResponse) {
  models.value = res.data
  providerMeta.value = res.providers ?? []
}

onMounted(() => load())

async function copy(text: string, id: string) {
  try {
    await navigator.clipboard.writeText(text)
    copied.value = id
    setTimeout(() => {
      if (copied.value === id) copied.value = null
    }, 1500)
  } catch {
    error.value = "Clipboard unavailable"
  }
}
</script>

<template>
  <div>
    <div class="page-header">
      <div>
        <h1 class="page-title">Models</h1>
        <p class="page-sub">
          Live from your bound provider accounts. Use ids as
          <code class="mono">provider/model</code>.
          <span v-if="fromCache" class="faint"> · showing cache</span>
        </p>
      </div>
      <button
        type="button"
        class="btn btn-secondary"
        :disabled="loading"
        @click="load({ force: true })"
      >
        Refresh
      </button>
    </div>

    <p v-if="error" class="banner error">{{ error }}</p>
    <p v-if="loading" class="muted">Loading…</p>

    <template v-else>
      <p class="faint" style="margin: 0 0 12px">
        {{ models.length }} model{{ models.length === 1 ? "" : "s" }} from your accounts
      </p>

      <div class="section-grid">
        <section v-for="group in grouped" :key="group.key" class="card models-card">
          <div class="models-card-head">
            <h2 class="models-provider">{{ group.name }}</h2>
            <span v-if="group.formatBadge" class="status-pill">{{ group.formatBadge }}</span>
          </div>
          <p v-if="group.blurb" class="muted" style="margin: 0 0 10px; font-size: 13px">
            {{ group.blurb }}
          </p>

          <p v-if="group.error" class="faint" style="margin: 0 0 8px; font-size: 12.5px">
            {{ group.error }}
          </p>

          <div v-if="group.models.length" class="models-table">
            <div class="models-row models-head">
              <span>Model id</span>
              <span>Name</span>
              <span />
            </div>
            <div v-for="m in group.models" :key="m.id" class="models-row">
              <code class="mono model-id">{{ m.id }}</code>
              <span>{{ m.display_name }}</span>
              <span class="models-actions">
                <button type="button" class="btn btn-secondary btn-sm" @click="copy(m.id, m.id)">
                  {{ copied === m.id ? "Copied" : "Copy id" }}
                </button>
              </span>
            </div>
          </div>
          <p v-else-if="group.emptyKind === 'codex'" class="faint" style="margin: 0; font-size: 12.5px">
            No models list for ChatGPT OAuth (not a Platform API key). See
            <a
              href="https://developers.openai.com/api/docs/models"
              target="_blank"
              rel="noopener"
            >OpenAI models</a>
            and
            <a
              href="https://learn.chatgpt.com/docs/models"
              target="_blank"
              rel="noopener"
            >ChatGPT / Codex models</a>.
          </p>
          <p v-else-if="group.emptyKind === 'custom'" class="faint" style="margin: 0; font-size: 12.5px">
            No models yet — open Edit on this endpoint (Accounts page) and add manual model ids,
            or switch it to auto-fetch.
          </p>
          <p v-else class="faint" style="margin: 0; font-size: 12.5px">
            No models
            <template v-if="!group.error">
              — bind an account on Accounts to load this provider’s models.
            </template>
          </p>
        </section>
      </div>
    </template>
  </div>
</template>

<style scoped>
.models-card {
  padding: 16px 18px;
}
.models-card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 4px;
}
.models-provider {
  margin: 0;
  font-size: 16px;
  font-weight: 600;
}
.models-table {
  display: flex;
  flex-direction: column;
  border: 1px solid var(--border);
  border-radius: 8px;
  overflow: hidden;
}
.models-row {
  display: grid;
  grid-template-columns: minmax(0, 1.6fr) minmax(0, 1fr) auto;
  gap: 10px;
  align-items: center;
  padding: 10px 12px;
  border-top: 1px solid var(--border);
  font-size: 13px;
}
.models-row:first-child {
  border-top: none;
}
.models-head {
  background: var(--surface-2);
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--muted);
}
.model-id {
  font-size: 12.5px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.models-actions {
  justify-self: end;
}
@media (max-width: 720px) {
  .models-row {
    grid-template-columns: 1fr;
  }
  .models-head {
    display: none;
  }
  .models-actions {
    justify-self: start;
  }
}
</style>
