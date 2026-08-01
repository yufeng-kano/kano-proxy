<script setup lang="ts">
import { onUnmounted, ref } from "vue"
import { completeLogin, startLogin } from "@/services/api"
import type { ProviderId } from "@/types"

const props = defineProps<{ provider: ProviderId; providerName: string }>()
const emit = defineEmits<{ close: []; added: [] }>()

const step = ref<"idle" | "claude" | "codex" | "grok" | "import">("idle")
const busy = ref(false)
const error = ref<string | null>(null)
const loginId = ref<string | null>(null)
const authUrl = ref<string | null>(null)
const userCode = ref<string | null>(null)
const verificationUri = ref<string | null>(null)
const pasteCode = ref("")
const importToken = ref("")
const importRefresh = ref("")
const importLabel = ref("")

let pollTimer: ReturnType<typeof setInterval> | null = null

function clearPoll() {
  if (pollTimer) {
    clearInterval(pollTimer)
    pollTimer = null
  }
}

onUnmounted(clearPoll)

async function beginOAuth() {
  busy.value = true
  error.value = null
  clearPoll()
  try {
    const res = await startLogin(props.provider)
    loginId.value = res.login_id

    if (props.provider === "claude-code") {
      step.value = "claude"
      authUrl.value = res.authorization_url ?? null
      if (authUrl.value) window.open(authUrl.value, "_blank", "noopener")
    } else if (props.provider === "codex") {
      step.value = "codex"
      authUrl.value = res.authorization_url ?? null
      if (authUrl.value) window.open(authUrl.value, "_blank", "noopener")
    } else if (props.provider === "grok") {
      step.value = "grok"
      userCode.value = res.user_code ?? null
      verificationUri.value =
        res.verification_uri_complete || res.verification_uri || null
      if (verificationUri.value) {
        window.open(verificationUri.value, "_blank", "noopener")
      }
      const intervalMs = Math.max(3, res.interval ?? 5) * 1000
      pollTimer = setInterval(() => {
        void pollGrok()
      }, intervalMs)
    }
  } catch (e) {
    error.value = e instanceof Error ? e.message : "Failed to start login"
  } finally {
    busy.value = false
  }
}

async function submitPasteComplete() {
  if (!loginId.value) return
  busy.value = true
  error.value = null
  try {
    const raw = pasteCode.value.trim()
    await completeLogin(props.provider, loginId.value, {
      // Claude: code#state · Codex: full localhost:1455 URL or code#state
      code: raw,
      value: raw,
    })
    emit("added")
    emit("close")
  } catch (e) {
    error.value = e instanceof Error ? e.message : "Complete failed"
  } finally {
    busy.value = false
  }
}

async function pollGrok() {
  if (!loginId.value || busy.value) return
  busy.value = true
  error.value = null
  try {
    await completeLogin(props.provider, loginId.value, {})
    clearPoll()
    emit("added")
    emit("close")
  } catch (e) {
    const msg = e instanceof Error ? e.message : "Waiting…"
    // device not ready is expected
    if (/authorization_pending|slow_down|token \d+/i.test(msg)) {
      error.value = "Waiting for device authorization…"
    } else {
      error.value = msg
    }
  } finally {
    busy.value = false
  }
}

async function submitImport() {
  busy.value = true
  error.value = null
  try {
    const { importAccount } = await import("@/services/api")
    if (!importToken.value.trim()) {
      error.value = "access_token required"
      return
    }
    await importAccount(props.provider, {
      access_token: importToken.value.trim(),
      refresh_token: importRefresh.value.trim() || undefined,
      label: importLabel.value.trim() || undefined,
    })
    emit("added")
    emit("close")
  } catch (e) {
    error.value = e instanceof Error ? e.message : "Import failed"
  } finally {
    busy.value = false
  }
}

function showImport() {
  clearPoll()
  step.value = "import"
  error.value = null
}
</script>

<template>
  <div class="overlay" @click.self="emit('close')">
    <div class="dialog" role="dialog" aria-modal="true">
      <h2>Add {{ providerName }} account</h2>

      <div v-if="step === 'idle'" class="stack">
        <p class="muted" style="margin: 0">
          Connect an upstream OAuth account to this provider pool.
        </p>
        <button type="button" class="btn" :disabled="busy" @click="beginOAuth">
          Start OAuth
        </button>
        <button type="button" class="btn btn-secondary" :disabled="busy" @click="showImport">
          Import tokens…
        </button>
      </div>

      <div v-else-if="step === 'claude'" class="stack">
        <p class="muted" style="margin: 0">
          1. Open authorize URL and approve.<br />
          2. On the Anthropic callback page, copy
          <code class="mono">code#state</code> and paste below.
        </p>
        <a
          v-if="authUrl"
          :href="authUrl"
          target="_blank"
          rel="noopener"
          class="btn btn-secondary"
        >
          Open authorize URL
        </a>
        <div class="field">
          <label for="paste-claude">code#state</label>
          <textarea
            id="paste-claude"
            v-model="pasteCode"
            class="input textarea"
            placeholder="xxxx#yyyy"
            autocomplete="off"
          />
        </div>
        <div class="dialog-actions">
          <button type="button" class="btn btn-ghost" @click="emit('close')">Cancel</button>
          <button
            type="button"
            class="btn"
            :disabled="busy || !pasteCode.trim()"
            @click="submitPasteComplete"
          >
            Complete
          </button>
        </div>
      </div>

      <div v-else-if="step === 'codex'" class="stack">
        <p class="muted" style="margin: 0">
          請留在錯誤頁，從網址列複製整段
          <code class="mono">http://localhost:1455/auth/callback?code=…</code>
          貼到下方，再按 Complete。
        </p>
        <a
          v-if="authUrl"
          :href="authUrl"
          target="_blank"
          rel="noopener"
          class="btn btn-secondary"
        >
          Open authorize URL
        </a>
        <div class="field">
          <label for="paste-codex">完整 callback URL</label>
          <textarea
            id="paste-codex"
            v-model="pasteCode"
            class="input textarea"
            placeholder="http://localhost:1455/auth/callback?code=…&state=…"
            autocomplete="off"
          />
        </div>
        <div class="dialog-actions">
          <button type="button" class="btn btn-ghost" @click="emit('close')">Cancel</button>
          <button
            type="button"
            class="btn"
            :disabled="busy || !pasteCode.trim()"
            @click="submitPasteComplete"
          >
            Complete
          </button>
        </div>
      </div>

      <div v-else-if="step === 'grok'" class="stack">
        <p class="muted" style="margin: 0">
          Enter this code on the xAI device page, then wait for confirmation.
        </p>
        <p v-if="userCode" class="mono" style="font-size: 22px; font-weight: 650; margin: 0">
          {{ userCode }}
        </p>
        <a
          v-if="verificationUri"
          :href="verificationUri"
          target="_blank"
          rel="noopener"
          class="btn btn-secondary"
        >
          Open verification page
        </a>
        <p class="faint" style="margin: 0">Polling for authorization…</p>
        <div class="dialog-actions">
          <button type="button" class="btn btn-ghost" @click="emit('close')">Cancel</button>
          <button type="button" class="btn" :disabled="busy" @click="pollGrok">
            Check now
          </button>
        </div>
      </div>

      <div v-else-if="step === 'import'" class="stack">
        <p class="muted" style="margin: 0">
          Manual credential import (bootstrap / recovery). Tokens are encrypted
          server-side; never store them in the browser.
        </p>
        <div class="field">
          <label for="at">access_token</label>
          <textarea id="at" v-model="importToken" class="input textarea" autocomplete="off" />
        </div>
        <div class="field">
          <label for="rt">refresh_token (optional)</label>
          <input id="rt" v-model="importRefresh" class="input" autocomplete="off" />
        </div>
        <div class="field">
          <label for="lb">label (optional)</label>
          <input id="lb" v-model="importLabel" class="input" autocomplete="off" />
        </div>
        <div class="dialog-actions">
          <button type="button" class="btn btn-ghost" @click="step = 'idle'">Back</button>
          <button type="button" class="btn" :disabled="busy" @click="submitImport">
            Import
          </button>
        </div>
      </div>

      <div v-if="error" class="banner error">{{ error }}</div>
    </div>
  </div>
</template>
