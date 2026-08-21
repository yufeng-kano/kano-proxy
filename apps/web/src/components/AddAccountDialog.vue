<script setup lang="ts">
/**
 * Connect one upstream subscription account.
 *
 * Two OAuth shapes behind one dialog. Claude Code and Antigravity open a
 * sign-in page and the user pastes back what the provider hands them — a code
 * for Claude, the dead localhost callback address for Antigravity, whose
 * client will not accept a redirect this proxy could host (docs/auth.md).
 * Codex and Grok are device flows the app polls until the user approves
 * elsewhere. The manual-token step is the escape hatch for credentials the
 * user already holds.
 *
 * Focus trap, Escape, and the mobile bottom sheet all come from Modal — this
 * component owns only the flow.
 */
import { computed, onUnmounted, ref } from "vue"
import { useI18n } from "@/i18n"
import { ApiError, completeLogin, importAccount, startLogin } from "@/services/api"
import type { ProviderId } from "@/types"
import AppButton from "./ui/AppButton.vue"
import Banner from "./ui/Banner.vue"
import FormField from "./ui/FormField.vue"
import Modal from "./ui/Modal.vue"
import TextInput from "./ui/TextInput.vue"

const props = defineProps<{ provider: ProviderId; providerName: string }>()
const emit = defineEmits<{ close: []; added: [] }>()

const { t } = useI18n()

/** `paste` covers both authorize-then-paste providers; the copy differs, the flow does not. */
type Step = "idle" | "paste" | "device" | "import"

const PASTE_PROVIDERS: ReadonlySet<string> = new Set(["claude-code", "antigravity"])

const step = ref<Step>("idle")
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

const title = computed(() =>
  step.value === "import"
    ? t("addAccount.manual.title")
    : t("addAccount.title", { provider: props.providerName }),
)

const pasteSteps = computed(() =>
  props.provider === "antigravity"
    ? [
        t("addAccount.antigravity.step1"),
        t("addAccount.antigravity.step2"),
        t("addAccount.antigravity.step3"),
      ]
    : [t("addAccount.claude.step1"), t("addAccount.claude.step2")],
)

const pasteLabel = computed(() =>
  props.provider === "antigravity"
    ? t("addAccount.antigravity.label")
    : t("addAccount.claude.label"),
)

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

    if (PASTE_PROVIDERS.has(props.provider)) {
      step.value = "paste"
      authUrl.value = res.authorization_url ?? null
      if (authUrl.value) window.open(authUrl.value, "_blank", "noopener")
    } else {
      // Codex and Grok are the same device flow, start to finish: show a code,
      // open the verification page, poll the same endpoint until approval.
      step.value = "device"
      userCode.value = res.user_code ?? null
      verificationUri.value = res.verification_uri_complete || res.verification_uri || null
      if (verificationUri.value) {
        window.open(verificationUri.value, "_blank", "noopener")
      }
      const intervalMs = Math.max(3, res.interval ?? 5) * 1000
      pollTimer = setInterval(() => {
        void pollDevice()
      }, intervalMs)
    }
  } catch (e) {
    // A 400 here is the server refusing on a precondition it can name — today,
    // Antigravity with no OAuth client configured. That sentence tells the
    // operator what to set; "Couldn't start sign-in" tells them nothing and
    // will keep telling them nothing on every retry. Anything else (network,
    // 5xx) stays generic, same as before.
    error.value =
      e instanceof ApiError && e.status === 400 && e.message
        ? e.message
        : t("addAccount.error.start")
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
      // Claude: code#state from the Anthropic callback page.
      // Antigravity: the whole localhost callback URL, or just its code.
      code: raw,
      value: raw,
    })
    emit("added")
    emit("close")
  } catch {
    error.value = t("addAccount.error.complete")
  } finally {
    busy.value = false
  }
}

async function pollDevice() {
  if (!loginId.value || busy.value) return
  busy.value = true
  error.value = null
  try {
    await completeLogin(props.provider, loginId.value, {})
    clearPoll()
    emit("added")
    emit("close")
  } catch (e) {
    const msg = e instanceof Error ? e.message : ""
    // Not-yet-approved is the expected answer on every tick until the user
    // acts on the other page — the step already says it is waiting, so a
    // banner repeating it as an error would be the app crying wolf.
    const pending = /authorization_pending|slow_down|token \d+/i.test(msg)
    error.value = pending ? null : t("addAccount.error.complete")
  } finally {
    busy.value = false
  }
}

async function submitImport() {
  if (!importToken.value.trim()) {
    error.value = t("addAccount.error.token")
    return
  }
  busy.value = true
  error.value = null
  try {
    await importAccount(props.provider, {
      access_token: importToken.value.trim(),
      refresh_token: importRefresh.value.trim() || undefined,
      label: importLabel.value.trim() || undefined,
    })
    emit("added")
    emit("close")
  } catch {
    error.value = t("addAccount.error.complete")
  } finally {
    busy.value = false
  }
}

function showImport() {
  clearPoll()
  step.value = "import"
  error.value = null
}

function backToStart() {
  step.value = "idle"
  error.value = null
}
</script>

<template>
  <Modal :title="title" @close="emit('close')">
    <div class="body">
      <template v-if="step === 'idle'">
        <p class="lede">{{ t("addAccount.intro", { provider: providerName }) }}</p>
      </template>

      <template v-else-if="step === 'paste'">
        <ol class="steps">
          <li v-for="(instruction, i) in pasteSteps" :key="i">{{ instruction }}</li>
        </ol>

        <AppButton v-if="authUrl" :href="authUrl">
          {{ t("addAccount.openAuth") }}
        </AppButton>

        <FormField v-slot="field" :label="pasteLabel">
          <TextInput
            :id="field.id"
            v-model="pasteCode"
            multiline
            mono
            :rows="3"
            :described-by="field.describedBy"
          />
        </FormField>
      </template>

      <template v-else-if="step === 'device'">
        <p class="lede">{{ t("addAccount.device.intro") }}</p>
        <p v-if="userCode" class="code mono">{{ userCode }}</p>
        <AppButton v-if="verificationUri" :href="verificationUri">
          {{ t("addAccount.openAuth") }}
        </AppButton>
        <!-- Polite, not assertive: this repeats on every poll tick, and an
             alert would re-announce "still waiting" every few seconds. -->
        <p class="hint" role="status" aria-live="polite">
          {{ t("addAccount.device.waiting") }}
        </p>
      </template>

      <template v-else>
        <p class="lede">{{ t("addAccount.manual.intro") }}</p>

        <FormField v-slot="field" :label="t('addAccount.manual.accessToken')">
          <TextInput
            :id="field.id"
            v-model="importToken"
            multiline
            mono
            :rows="3"
            :described-by="field.describedBy"
          />
        </FormField>

        <FormField
          v-slot="field"
          :label="t('addAccount.manual.refreshToken')"
          :optional-text="t('addAccount.manual.optional')"
        >
          <TextInput
            :id="field.id"
            v-model="importRefresh"
            mono
            :described-by="field.describedBy"
          />
        </FormField>

        <FormField
          v-slot="field"
          :label="t('addAccount.manual.label')"
          :optional-text="t('addAccount.manual.optional')"
        >
          <TextInput :id="field.id" v-model="importLabel" :described-by="field.describedBy" />
        </FormField>
      </template>

      <Banner v-if="error" tone="error">{{ error }}</Banner>
    </div>

    <template #footer>
      <template v-if="step === 'idle'">
        <AppButton variant="ghost" :disabled="busy" @click="showImport">
          {{ t("addAccount.manual") }}
        </AppButton>
        <AppButton variant="primary" :loading="busy" @click="beginOAuth">
          {{ t("addAccount.start") }}
        </AppButton>
      </template>

      <template v-else-if="step === 'paste'">
        <AppButton variant="ghost" @click="emit('close')">{{ t("action.cancel") }}</AppButton>
        <AppButton
          variant="primary"
          :loading="busy"
          :disabled="!pasteCode.trim()"
          @click="submitPasteComplete"
        >
          {{ t("addAccount.complete") }}
        </AppButton>
      </template>

      <template v-else-if="step === 'device'">
        <AppButton variant="ghost" @click="emit('close')">{{ t("action.cancel") }}</AppButton>
        <AppButton variant="primary" :loading="busy" @click="pollDevice">
          {{ t("addAccount.device.check") }}
        </AppButton>
      </template>

      <template v-else>
        <AppButton variant="ghost" :disabled="busy" @click="backToStart">
          {{ t("action.back") }}
        </AppButton>
        <AppButton variant="primary" :loading="busy" @click="submitImport">
          {{ t("addAccount.manual.submit") }}
        </AppButton>
      </template>
    </template>
  </Modal>
</template>

<style scoped>
.body {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

.lede {
  margin: 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.6;
}

.steps {
  display: grid;
  gap: var(--space-2);
  margin: 0;
  padding-left: var(--space-5);
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.6;
}

/* The device code is the thing the user retypes elsewhere, so it gets the
   largest type in the dialog and the widest tracking the mono face allows. */
.code {
  margin: 0;
  padding: var(--space-3);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface-2);
  color: var(--text);
  font-size: var(--text-xl);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-wide);
  text-align: center;
  overflow-wrap: anywhere;
}

.hint {
  margin: 0;
  color: var(--muted);
  font-size: var(--text-xs);
}
</style>
