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
import {
  PROVIDERS,
  type CatalogModel,
  type ModelsResponse,
  type ProviderId,
} from "@/types"

const { user } = useAuth()
const models = ref<CatalogModel[]>([])
const providerMeta = ref<
  Array<{ provider: ProviderId; count: number; error: string | null; cached: boolean }>
>([])
const loading = ref(true)
const error = ref<string | null>(null)
const copied = ref<string | null>(null)
const fromCache = ref(false)

const grouped = computed(() => {
  const map = new Map<ProviderId, CatalogModel[]>()
  for (const p of PROVIDERS) map.set(p.id, [])
  for (const m of models.value) {
    const list = map.get(m.provider) ?? []
    list.push(m)
    map.set(m.provider, list)
  }
  return PROVIDERS.map((p) => {
    const meta = providerMeta.value.find((x) => x.provider === p.id)
    return {
      ...p,
      models: map.get(p.id) ?? [],
      error: meta?.error ?? null,
      cached: meta?.cached ?? false,
    }
  })
})

async function load(opts?: { force?: boolean }) {
  const uid = user.value?.id ?? null
  error.value = null

  if (!opts?.force) {
    const cached = readModelsCache(uid)
    if (cached) {
      applyResponse(cached)
      fromCache.value = true
      loading.value = false
      if (isModelsCacheFresh(uid, CACHE_TTL_MS)) return
    }
  }

  if (!models.value.length) loading.value = true
  try {
    const res = await listModels({ refresh: !!opts?.force })
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
        <section v-for="group in grouped" :key="group.id" class="card models-card">
          <div class="models-card-head">
            <h2 class="models-provider">{{ group.name }}</h2>
          </div>
          <p class="muted" style="margin: 0 0 10px; font-size: 13px">{{ group.blurb }}</p>

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
          <p v-else class="faint" style="margin: 0; font-size: 12.5px">
            No models
            <template v-if="!group.error">
              — bind an account on Accounts, or this provider has no models API.
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
  align-items: baseline;
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
