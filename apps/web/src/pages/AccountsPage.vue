<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue"
import AccountCard from "@/components/AccountCard.vue"
import AddAccountDialog from "@/components/AddAccountDialog.vue"
import CustomProviderCard from "@/components/CustomProviderCard.vue"
import CustomProviderDialog from "@/components/CustomProviderDialog.vue"
import { useAccounts } from "@/composables/useAccounts"
import { useAuth } from "@/composables/useAuth"
import { useCustomProviders } from "@/composables/useCustomProviders"
import { deleteCustomProvider, promoteAccount, removeAccount } from "@/services/api"
import { PROVIDERS, type CustomProvider, type ProviderId } from "@/types"

const { user } = useAuth()
const { byProvider, setUserId, loadAll, loadProvider, CACHE_TTL_MS } = useAccounts()
const customProviders = useCustomProviders()

const busyId = ref<string | null>(null)
const actionError = ref<string | null>(null)
const addFor = ref<{ id: ProviderId; name: string } | null>(null)

const customBusyId = ref<string | null>(null)
const showCustomDialog = ref(false)
const editingCustomProvider = ref<CustomProvider | null>(null)

let pollTimer: number | undefined

onMounted(async () => {
  setUserId(user.value?.id ?? null)
  customProviders.setUserId(user.value?.id ?? null)
  // Cache-first: paint localStorage, network only if >90s stale
  await Promise.all([loadAll(), customProviders.load()])
  // Background poll every 90s (same as the local cache TTL) — no force
  pollTimer = window.setInterval(() => {
    void loadAll()
    void customProviders.load()
  }, CACHE_TTL_MS)
})

onUnmounted(() => {
  if (pollTimer !== undefined) window.clearInterval(pollTimer)
})

async function refreshAll() {
  actionError.value = null
  // Manual: skip the local cache and force a network fetch
  await Promise.all([loadAll({ refresh: true }), customProviders.load({ refresh: true })])
}

function openCreateCustomDialog() {
  editingCustomProvider.value = null
  showCustomDialog.value = true
}

function openEditCustomDialog(p: CustomProvider) {
  editingCustomProvider.value = p
  showCustomDialog.value = true
}

function closeCustomDialog() {
  showCustomDialog.value = false
  editingCustomProvider.value = null
}

async function onCustomProviderSaved() {
  await customProviders.load({ refresh: true })
}

async function onRemoveCustomProvider(p: CustomProvider) {
  if (!confirm(`Remove "${p.name}"? This also deletes its stored API key.`)) return
  customBusyId.value = p.id
  actionError.value = null
  try {
    await deleteCustomProvider(p.id)
    await customProviders.load({ refresh: true })
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : "Remove failed"
  } finally {
    customBusyId.value = null
  }
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
          (local 90s cache + 90s poll) to avoid 429.
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

      <section class="card provider-card">
        <div class="provider-head">
          <div>
            <h2 class="provider-title">Custom endpoints</h2>
            <p class="provider-blurb">Bring your own OpenAI- or Anthropic-compatible endpoint.</p>
          </div>
          <div class="row-gap">
            <span v-if="customProviders.state.fromCache" class="faint" style="font-size: 12px">
              cached
            </span>
            <span v-if="customProviders.state.refreshing" class="faint" style="font-size: 12px">
              refreshing…
            </span>
            <button type="button" class="btn btn-secondary btn-sm" @click="openCreateCustomDialog">
              Add endpoint
            </button>
          </div>
        </div>

        <div class="provider-body">
          <div v-if="customProviders.state.error" class="banner warn">
            {{ customProviders.state.error }}
            <span v-if="customProviders.state.data"> — showing cached data</span>
          </div>

          <div
            v-if="customProviders.state.loading && !customProviders.state.data"
            class="empty"
          >
            Loading…
          </div>

          <template v-else-if="customProviders.state.data?.length">
            <CustomProviderCard
              v-for="p in customProviders.state.data"
              :key="p.id"
              :provider="p"
              :busy="customBusyId === p.id"
              @edit="openEditCustomDialog(p)"
              @remove="onRemoveCustomProvider(p)"
            />
          </template>

          <div v-else class="empty">
            No custom endpoints yet. Add an OpenAI- or Anthropic-compatible endpoint to route
            requests to it as <code class="mono">slug/model</code>.
          </div>
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

    <CustomProviderDialog
      v-if="showCustomDialog"
      :provider="editingCustomProvider"
      @close="closeCustomDialog"
      @saved="onCustomProviderSaved"
    />
  </div>
</template>
