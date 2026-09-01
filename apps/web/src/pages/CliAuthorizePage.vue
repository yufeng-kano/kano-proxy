<script setup lang="ts">
/**
 * Approve view for a pending `kano-proxy init` login (docs/cli.md § Web UI,
 * docs/admin-ui.md § CLI authorize view). Session-gated by the router like
 * every admin page, but rendered bare — no shell, a blank page with one
 * centered card: the person landing here came from a terminal to answer one
 * question. On approve the one-time code renders exactly once — the server
 * stores only its hash, so a refresh lands on the already-approved state,
 * never the code again.
 */
import { onMounted, ref } from "vue"
import { useRoute } from "vue-router"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Banner from "@/components/ui/Banner.vue"
import CopyField from "@/components/ui/CopyField.vue"
import Spinner from "@/components/ui/Spinner.vue"
import { SITE } from "@/config/site"
import { useI18n } from "@/i18n"
import { approveCliLoginRequest, denyCliLoginRequest, getCliLoginRequest } from "@/services/api"
import type { CliLoginRequest } from "@/types"

type View = "loading" | "pending" | "code" | "already-approved" | "denied" | "missing"

const { t } = useI18n()
const route = useRoute()

const view = ref<View>("loading")
const request = ref<CliLoginRequest | null>(null)
const code = ref("")
const busy = ref(false)
const error = ref<string | null>(null)

const requestId = typeof route.query.request === "string" ? route.query.request : ""

onMounted(async () => {
  if (!requestId) {
    view.value = "missing"
    return
  }
  try {
    const row = await getCliLoginRequest(requestId)
    request.value = row
    view.value = row.approved ? "already-approved" : "pending"
  } catch {
    view.value = "missing"
  }
})

async function approve() {
  busy.value = true
  error.value = null
  try {
    code.value = await approveCliLoginRequest(requestId)
    view.value = "code"
  } catch {
    error.value = t("cli.authorize.error.approve")
  } finally {
    busy.value = false
  }
}

async function deny() {
  busy.value = true
  error.value = null
  try {
    await denyCliLoginRequest(requestId)
    view.value = "denied"
  } catch {
    error.value = t("cli.authorize.error.deny")
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="page">
    <AppCard class="panel">
      <div class="brand">
        <span class="brand-mark" aria-hidden="true">k</span>
        <span class="brand-name">{{ SITE.name }}</span>
      </div>

      <h1 class="title">{{ t("cli.authorize.title") }}</h1>

      <Banner v-if="error" tone="error">
        {{ error }}
        <template #actions>
          <AppButton size="sm" variant="ghost" @click="error = null">
            {{ t("action.dismiss") }}
          </AppButton>
        </template>
      </Banner>

      <div v-if="view === 'loading'" class="state" role="status">
        <Spinner />
        <span class="sr-only">{{ t("app.loading") }}</span>
      </div>

      <div v-else-if="view === 'pending'" class="state">
        <p class="question">{{ t("cli.authorize.question", { device: request?.device_name ?? "" }) }}</p>
        <p class="hint">{{ t("cli.authorize.hint") }}</p>
        <div class="actions">
          <AppButton variant="ghost" :disabled="busy" @click="deny">
            {{ t("cli.authorize.deny") }}
          </AppButton>
          <AppButton variant="primary" :loading="busy" @click="approve">
            {{ t("cli.authorize.approve") }}
          </AppButton>
        </div>
      </div>

      <div v-else-if="view === 'code'" class="state">
        <p class="question">{{ t("cli.authorize.codeTitle") }}</p>
        <CopyField class="code" :value="code" emphasis @error="error = $event" />
        <p class="hint">{{ t("cli.authorize.codeHint") }}</p>
      </div>

      <div v-else class="state">
        <p class="question">
          {{
            view === "already-approved"
              ? t("cli.authorize.alreadyApproved")
              : view === "denied"
                ? t("cli.authorize.denied")
                : t("cli.authorize.missing")
          }}
        </p>
        <AppButton variant="ghost" to="/providers">{{ t("cli.authorize.goToApp") }}</AppButton>
      </div>
    </AppCard>
  </div>
</template>

<style scoped>
/* The bare surface: the shell never mounts on this route, so the page paints
   its own full-height ground and centers the one card on it. */
.page {
  display: grid;
  place-items: center;
  min-height: 100dvh;
  padding: var(--space-4);
  background: var(--bg);
}

.panel {
  width: 100%;
  max-width: 440px;
}

.brand {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-bottom: var(--space-5);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
}

.brand-mark {
  display: inline-grid;
  place-items: center;
  width: 24px;
  height: 24px;
  border-radius: var(--radius-sm);
  background: var(--accent);
  color: var(--accent-fg);
  font-size: var(--text-xs);
  font-weight: var(--weight-bold);
}

.brand-name {
  font-size: var(--text-sm);
}

.title {
  margin: 0 0 var(--space-4);
  font-size: var(--text-lg);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
}

.state {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: var(--space-4);
}

.question {
  margin: 0;
  font-size: var(--text-md);
  font-weight: var(--weight-medium);
}

.hint {
  margin: 0;
  color: var(--muted);
  font-size: var(--text-xs);
}

.actions {
  display: flex;
  gap: var(--space-2);
}

.code {
  width: 100%;
}

.code :deep(.copy-value) {
  font-size: var(--text-lg);
  letter-spacing: 0.12em;
}
</style>
