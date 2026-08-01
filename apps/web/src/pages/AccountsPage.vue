<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue"
import AccountCard from "@/components/AccountCard.vue"
import AddAccountDialog from "@/components/AddAccountDialog.vue"
import { useAccounts } from "@/composables/useAccounts"
import { useAuth } from "@/composables/useAuth"
import { promoteAccount, removeAccount } from "@/services/api"
import { PROVIDERS, type ProviderId } from "@/types"

const { user } = useAuth()
const { byProvider, setUserId, loadAll, loadProvider, CACHE_TTL_MS } = useAccounts()

const busyId = ref<string | null>(null)
const actionError = ref<string | null>(null)
const addFor = ref<{ id: ProviderId; name: string } | null>(null)

let pollTimer: number | undefined

onMounted(async () => {
  setUserId(user.value?.id ?? null)
  // Cache-first: paint sessionStorage, network only if >90s stale
  await loadAll()
  // Background poll every 90s (same as usage cache TTL) — no force, so server KV also helps
  pollTimer = window.setInterval(() => {
    void loadAll()
  }, CACHE_TTL_MS)
})

onUnmounted(() => {
  if (pollTimer !== undefined) window.clearInterval(pollTimer)
})

async function refreshAll() {
  actionError.value = null
  // Manual: bypass frontend + backend 90s caches
  await loadAll({ refresh: true })
}

async function onPromote(provider: ProviderId, id: string) {
  busyId.value = id
  actionError.value = null
  try {
    await promoteAccount(provider, id)
    await loadProvider(provider, { refresh: true })
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : "Promote failed"
  } finally {
    busyId.value = null
  }
}

async function onRemove(provider: ProviderId, id: string) {
  if (!confirm("Remove this account from the pool?")) return
  busyId.value = id
  actionError.value = null
  try {
    await removeAccount(provider, id)
    await loadProvider(provider, { refresh: true })
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : "Remove failed"
  } finally {
    busyId.value = null
  }
}

async function onAdded(provider: ProviderId) {
  addFor.value = null
  await loadProvider(provider, { refresh: true })
}
</script>

<template>
  <div>
    <div class="page-header">
      <div>
        <h1 class="page-title">Accounts</h1>
        <p class="page-sub">
          Upstream pools for Claude Code, Codex, and Grok. Usage is cache-first
          (sessionStorage + server 90s) to avoid 429.
        </p>
      </div>
      <button type="button" class="btn btn-secondary" @click="refreshAll">
        Refresh usage
      </button>
    </div>

    <div v-if="actionError" class="banner error" style="margin-bottom: 16px">
      {{ actionError }}
    </div>

    <div class="section-grid">
      <section
        v-for="p in PROVIDERS"
        :key="p.id"
        class="card provider-card"
      >
        <div class="provider-head">
          <div>
            <h2 class="provider-title">{{ p.name }}</h2>
            <p class="provider-blurb">{{ p.blurb }}</p>
          </div>
          <div class="row-gap">
            <span
              v-if="byProvider[p.id].fromCache"
              class="faint"
              style="font-size: 12px"
            >
              cached
            </span>
            <span
              v-if="byProvider[p.id].refreshing"
              class="faint"
              style="font-size: 12px"
            >
              refreshing…
            </span>
            <button
              type="button"
              class="btn btn-secondary btn-sm"
              @click="addFor = { id: p.id, name: p.name }"
            >
              Add account
            </button>
          </div>
        </div>

        <div class="provider-body">
          <div
            v-if="byProvider[p.id].error"
            class="banner warn"
          >
            {{ byProvider[p.id].error }}
            <span v-if="byProvider[p.id].data"> — showing cached data</span>
          </div>

          <div v-if="byProvider[p.id].loading && !byProvider[p.id].data" class="empty">
            Loading…
          </div>

          <template v-else-if="byProvider[p.id].data?.accounts?.length">
            <AccountCard
              v-for="acc in byProvider[p.id].data!.accounts"
              :key="acc.id"
              :account="acc"
              :busy="busyId === acc.id"
              @promote="onPromote(p.id, acc.id)"
              @remove="onRemove(p.id, acc.id)"
            />
          </template>

          <div v-else class="empty">No accounts yet. Add one to start routing.</div>
        </div>
      </section>
    </div>

    <AddAccountDialog
      v-if="addFor"
      :provider="addFor.id"
      :provider-name="addFor.name"
      @close="addFor = null"
      @added="onAdded(addFor!.id)"
    />
  </div>
</template>
