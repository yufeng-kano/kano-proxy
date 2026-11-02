<script setup lang="ts">
/**
 * The usage notice a signed-in user accepts before any other page, and again
 * when it changes (docs/admin-ui.md § Usage notice page, docs/auth.md § Usage
 * notice). It sits on the sign-in page's layout because it is the last step of
 * signing in, not a page of the app.
 */
import { computed, ref } from "vue"
import { useRoute, useRouter } from "vue-router"
import SignInLayout from "@/components/SignInLayout.vue"
import AppButton from "@/components/ui/AppButton.vue"
import Banner from "@/components/ui/Banner.vue"
import { useAuth } from "@/composables/useAuth"
import { useWebExtensions } from "@/extensions"
import { useI18n, type MessageKey } from "@/i18n"
import { ApiError } from "@/services/api"

const { t, locale, format } = useI18n()
const { user, notice, acceptNotice, refresh, logout } = useAuth()
const { noticeTerms = [] } = useWebExtensions()
const route = useRoute()
const router = useRouter()

/** The six points, in the notice's order. Each opens with its bold lead-in. */
const POINTS: readonly { lead: MessageKey; body: MessageKey }[] = [
  { lead: "notice.affiliation.lead", body: "notice.affiliation.body" },
  { lead: "notice.signIn.lead", body: "notice.signIn.body" },
  { lead: "notice.restricted.lead", body: "notice.restricted.body" },
  { lead: "notice.ownAccounts.lead", body: "notice.ownAccounts.body" },
  { lead: "notice.endpoints.lead", body: "notice.endpoints.body" },
  { lead: "notice.asIs.lead", body: "notice.asIs.body" },
]

const agreed = ref(false)
const saving = ref(false)
const failure = ref<"changed" | "save" | null>(null)

/**
 * The edition's documents joined as the locale lists them ("A and B"), with
 * each name a link. `formatToParts` keeps the separators the locale's own.
 */
const termsParts = computed(() =>
  new Intl.ListFormat(locale.value, { type: "conjunction" })
    .formatToParts(noticeTerms.map(link => link.label))
    .map((part, i, parts) => ({
      text: part.value,
      link: part.type === "element" ? noticeTerms[parts.slice(0, i).filter(p => p.type === "element").length] : null,
    })),
)

/**
 * `YYYY-MM-DD` read as a local calendar date. `new Date("2026-11-02")` is UTC
 * midnight, which prints as the day before anywhere west of Greenwich.
 */
const updated = computed(() => {
  if (!notice.value) return ""
  const [year, month, day] = notice.value.updated.split("-").map(Number)
  return format.date(new Date(year, month - 1, day))
})

const redirect = computed(() => {
  const target = route.query.redirect
  return typeof target === "string" && target.startsWith("/") ? target : "/"
})

async function agree() {
  if (!notice.value || !agreed.value || saving.value) return
  saving.value = true
  failure.value = null
  try {
    await acceptNotice(notice.value.version)
    await router.replace(redirect.value)
  } catch (e) {
    if (e instanceof ApiError && e.status === 409) {
      // The text changed while this page was open: show the new one and ask again.
      await refresh()
      agreed.value = false
      failure.value = "changed"
    } else {
      failure.value = "save"
    }
  } finally {
    saving.value = false
  }
}

async function signOut() {
  await logout()
  await router.replace({ name: "login" })
}
</script>

<template>
  <SignInLayout wide>
    <template #brand>
      <p v-if="user" class="notice-account">
        <span class="notice-avatar" aria-hidden="true">{{ (user.name || user.email).slice(0, 1).toUpperCase() }}</span>
        <span>
          {{ t("login.signedInAs") }}
          <span class="notice-email">{{ user.email }}</span>
        </span>
      </p>
    </template>

    <form class="notice" @submit.prevent="agree">
      <div>
        <h1>{{ t("notice.title") }}</h1>
        <p class="notice-lede">{{ t("notice.lede") }}</p>
      </div>

      <ol class="notice-points">
        <li v-for="point in POINTS" :key="point.lead">
          <strong>{{ t(point.lead) }}</strong>
          {{ " " + t(point.body) }}
        </li>
      </ol>

      <div class="notice-agree">
        <input id="notice-agree" v-model="agreed" type="checkbox" />
        <label for="notice-agree">
          <template v-if="noticeTerms.length === 0">{{ t("notice.agree") }}</template>
          <template v-else>
            {{ t("notice.agreeTo") + " " }}
            <template v-for="(part, i) in termsParts" :key="i">
              <!-- A new tab, so the ticked box and this page stay where they are. -->
              <a v-if="part.link" :href="part.link.href" target="_blank" rel="noopener">{{ part.text }}</a>
              <template v-else>{{ part.text }}</template>
            </template>.
          </template>
        </label>
      </div>

      <Banner v-if="failure === 'changed'" tone="warn">{{ t("notice.error.changed") }}</Banner>
      <Banner v-else-if="failure === 'save'" tone="error">{{ t("notice.error.save") }}</Banner>

      <div class="notice-actions">
        <AppButton type="submit" variant="primary" :disabled="!agreed || !notice" :loading="saving">
          {{ t("notice.continue") }}
        </AppButton>
        <AppButton variant="secondary" @click="signOut">{{ t("notice.signOut") }}</AppButton>
      </div>

      <p v-if="notice" class="notice-updated">{{ t("notice.updated", { date: updated }) }}</p>
    </form>
  </SignInLayout>
</template>

<style scoped>
.notice-account {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  margin: 0;
  padding-top: var(--space-5);
  border-top: 1px solid var(--brand-line);
  color: var(--brand-muted);
  font-size: var(--text-sm);
}

/* The account's initial, like the shell's user block. 28px is an avatar size, not spacing. */
.notice-avatar {
  display: inline-flex;
  flex-shrink: 0;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border-radius: var(--radius-full);
  background: rgb(255 255 255 / 12%);
  color: var(--brand-text);
  font-size: var(--text-xs);
  font-weight: var(--weight-semibold);
}

.notice-email {
  color: var(--brand-text);
  font-weight: var(--weight-medium);
  overflow-wrap: anywhere;
}

.notice {
  display: grid;
  gap: var(--space-5);
}

h1 {
  margin: 0 0 var(--space-1);
  font-size: var(--text-xl);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tighter);
}

.notice-lede {
  margin: 0;
  color: var(--muted);
  font-size: var(--text-base);
  line-height: 1.55;
}

.notice-points {
  display: grid;
  gap: var(--space-3);
  margin: 0;
  padding: var(--space-4) var(--space-4) var(--space-4) var(--space-8);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface-2);
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.55;
}

.notice-points strong {
  color: var(--text);
  font-weight: var(--weight-semibold);
}

.notice-agree {
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
  font-size: var(--text-sm);
  line-height: 1.5;
}

/* Lined up with the label's first line. 16px is the control's own size. */
.notice-agree input {
  flex-shrink: 0;
  width: 16px;
  height: 16px;
  margin: 2px 0 0;
  accent-color: var(--text);
}

.notice-agree a {
  color: var(--text);
  text-decoration: underline;
  text-underline-offset: 2px;
}

.notice-actions {
  display: grid;
  gap: var(--space-2);
}

.notice-updated {
  margin: 0;
  color: var(--muted);
  font-size: var(--text-xs);
}
</style>
