<script setup lang="ts">
import { onMounted, ref } from "vue"
import {
  clientBaseUrls,
  createKey,
  listKeys,
  revokeKey,
} from "@/services/api"
import type { ApiKey, CreatedKey } from "@/types"

const keys = ref<ApiKey[]>([])
const loading = ref(true)
const error = ref<string | null>(null)
const creating = ref(false)
const name = ref("")
const created = ref<CreatedKey | null>(null)
const copied = ref<string | null>(null)
const baseUrls = clientBaseUrls()


async function load() {
  loading.value = true
  error.value = null
  try {
    keys.value = await listKeys()
  } catch (e) {
    error.value = e instanceof Error ? e.message : "Failed to load keys"
  } finally {
    loading.value = false
  }
}

onMounted(load)

async function onCreate() {
  creating.value = true
  error.value = null
  try {
    created.value = await createKey(name.value || "default")
    name.value = ""
    await load()
  } catch (e) {
    error.value = e instanceof Error ? e.message : "Create failed"
  } finally {
    creating.value = false
  }
}

async function onRevoke(id: string) {
  if (!confirm("Revoke this API key? Clients using it will fail immediately.")) return
  error.value = null
  try {
    await revokeKey(id)
    if (created.value?.id === id) created.value = null
    await load()
  } catch (e) {
    error.value = e instanceof Error ? e.message : "Revoke failed"
  }
}

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

function formatDate(iso: string | null) {
  if (!iso) return "—"
  try {
    return new Date(iso).toLocaleString()
  } catch {
    return iso
  }
}
</script>

<template>
  <div>
    <div class="page-header">
      <div>
        <h1 class="page-title">API keys</h1>
        <p class="page-sub">
          Project-issued keys for LLM clients. Plaintext is shown once at
          creation; only a prefix is stored for display.
        </p>
      </div>
    </div>

    <div class="stack" style="margin-bottom: 24px">
      <div class="card card-pad stack">
        <h2 class="provider-title" style="margin: 0">Client base URLs</h2>
        <div class="copy-block">
          <span class="label">OpenAI</span>
          <code>{{ baseUrls.openai }}</code>
          <button
            type="button"
            class="btn btn-secondary btn-sm"
            @click="copy(baseUrls.openai, 'openai')"
          >
            {{ copied === "openai" ? "Copied" : "Copy" }}
          </button>
        </div>
        <div class="copy-block">
          <span class="label">Anthropic</span>
          <code>{{ baseUrls.anthropic }}</code>
          <button
            type="button"
            class="btn btn-secondary btn-sm"
            @click="copy(baseUrls.anthropic, 'anthropic')"
          >
            {{ copied === "anthropic" ? "Copied" : "Copy" }}
          </button>
        </div>
        <p class="faint" style="margin: 0; font-size: 12.5px">
          Authenticate with
          <code class="mono">Authorization: Bearer sk-kano-proxy-…</code>
          or <code class="mono">x-api-key</code>.
        </p>
      </div>
    </div>

    <div v-if="error" class="banner error" style="margin-bottom: 16px">
      {{ error }}
    </div>

    <div
      v-if="created"
      class="banner ok stack"
      style="margin-bottom: 16px; gap: 8px"
    >
      <strong>New key created — copy it now. It will not be shown again.</strong>
      <div class="copy-block" style="background: var(--surface)">
        <code style="color: var(--text)">{{ created.key }}</code>
        <button
          type="button"
          class="btn btn-secondary btn-sm"
          @click="copy(created!.key, 'newkey')"
        >
          {{ copied === "newkey" ? "Copied" : "Copy" }}
        </button>
      </div>
      <button type="button" class="btn btn-ghost btn-sm" style="justify-self: start" @click="created = null">
        Dismiss
      </button>
    </div>

    <div class="card" style="margin-bottom: 20px">
      <div class="card-pad" style="display: flex; gap: 10px; flex-wrap: wrap; align-items: end">
        <div class="field" style="flex: 1; min-width: 180px">
          <label for="key-name">Name</label>
          <input
            id="key-name"
            v-model="name"
            class="input"
            placeholder="default"
            autocomplete="off"
            @keydown.enter="onCreate"
          />
        </div>
        <button type="button" class="btn" :disabled="creating" @click="onCreate">
          {{ creating ? "Creating…" : "Create key" }}
        </button>
      </div>
    </div>

    <div class="card" style="overflow: hidden">
      <div v-if="loading" class="empty">Loading…</div>
      <div v-else-if="!keys.length" class="empty">No keys yet.</div>
      <div v-else class="table-scroll">
        <table class="key-table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Prefix</th>
              <th>Created</th>
              <th>Last used</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="k in keys" :key="k.id">
              <td>{{ k.name }}</td>
              <td class="mono">{{ k.key_prefix }}…</td>
              <td class="muted">{{ formatDate(k.created_at) }}</td>
              <td class="muted">{{ formatDate(k.last_used_at) }}</td>
              <td style="text-align: right">
                <button
                  type="button"
                  class="btn btn-danger btn-sm"
                  @click="onRevoke(k.id)"
                >
                  Revoke
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </div>
</template>
