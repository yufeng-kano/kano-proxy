<script setup lang="ts">
/**
 * The one surface a signed-out visitor sees: the shared two-column layout with
 * the provider list under the pitch and the Google button in the panel.
 *
 * The Google button is the other prescribed surface: white background,
 * `#dadce0` border, the official four-color mark inlined as SVG (never a remote
 * asset), and Google's own dark variant. Those values are Google's to set, not
 * ours to tokenize.
 */
import { useRoute } from "vue-router"
import { useAuth } from "@/composables/useAuth"
import SignInLayout from "@/components/SignInLayout.vue"
import Banner from "@/components/ui/Banner.vue"
import { useI18n } from "@/i18n"
import type { MessageKey } from "@/i18n"
import { PROVIDERS, type ProviderId } from "@/types"

const { t } = useI18n()
const { goLogin, error } = useAuth()
const route = useRoute()

/** The guard's intended destination, carried through the OAuth round trip. */
function signIn() {
  const redirect = typeof route.query.redirect === "string" ? route.query.redirect : null
  goLogin(redirect)
}

/**
 * Provider display copy lives in the catalog, and `PROVIDERS` carries only wire
 * ids. An explicit map rather than a template literal: `` `provider.${id}.name` ``
 * widens to `string` and would not typecheck against `MessageKey`, so a
 * renamed key has to fail the build here.
 */
const NAME_KEY: Record<ProviderId, MessageKey> = {
  "claude-code": "provider.claude-code.name",
  codex: "provider.codex.name",
  grok: "provider.grok.name",
  antigravity: "provider.antigravity.name",
}

const BLURB_KEY: Record<ProviderId, MessageKey> = {
  "claude-code": "provider.claude-code.blurb",
  codex: "provider.codex.blurb",
  grok: "provider.grok.blurb",
  antigravity: "provider.antigravity.blurb",
}
</script>

<template>
  <SignInLayout>
    <template #brand>
      <ul class="login-providers">
        <li v-for="p in PROVIDERS" :key="p.id">
          <span class="login-provider-name">{{ t(NAME_KEY[p.id]) }}</span>
          <span class="login-provider-blurb">{{ t(BLURB_KEY[p.id]) }}</span>
        </li>
      </ul>
    </template>

    <h1>{{ t("login.signIn") }}</h1>
    <p class="login-lede">{{ t("login.lede") }}</p>

    <button type="button" class="btn-google" @click="signIn">
      <svg
        class="btn-google-mark"
        viewBox="0 0 18 18"
        aria-hidden="true"
        focusable="false"
      >
        <path
          fill="#4285F4"
          d="M17.64 9.2045c0-.6381-.0573-1.2518-.1636-1.8409H9v3.4814h4.8436c-.2086 1.125-.8427 2.0782-1.7959 2.7164v2.2581h2.9087c1.7018-1.5668 2.6836-3.874 2.6836-6.615z"
        />
        <path
          fill="#34A853"
          d="M9 18c2.43 0 4.4673-.806 5.9564-2.1805l-2.9087-2.2581c-.8059.54-1.8368.859-3.0477.859-2.344 0-4.3282-1.5831-5.036-3.7104H.9574v2.3318C2.4382 15.9832 5.4818 18 9 18z"
        />
        <path
          fill="#FBBC05"
          d="M3.964 10.71c-.18-.54-.2822-1.1168-.2822-1.71s.1023-1.17.2823-1.71V4.9582H.9573A8.9965 8.9965 0 0 0 0 9c0 1.4523.3477 2.8268.9573 4.0418L3.964 10.71z"
        />
        <path
          fill="#EA4335"
          d="M9 3.5795c1.3214 0 2.5077.4541 3.4405 1.346l2.5813-2.5814C13.4632.8918 11.426 0 9 0 5.4818 0 2.4382 2.0168.9573 4.9582L3.964 7.29C4.6718 5.1627 6.6559 3.5795 9 3.5795z"
        />
      </svg>
      <span>{{ t("login.google") }}</span>
    </button>

    <div v-if="error" class="login-error">
      <Banner tone="error">{{ t("login.error") }}</Banner>
    </div>
  </SignInLayout>
</template>

<style scoped>
.login-providers {
  display: grid;
  margin: 0;
  padding: var(--space-5) 0 0;
  list-style: none;
  border-top: 1px solid var(--brand-line);
}

.login-providers li {
  display: flex;
  align-items: baseline;
  gap: var(--space-3);
  padding: var(--space-2) 0;
  font-size: var(--text-sm);
}

/* A label track, so the blurbs line up regardless of name length. */
.login-provider-name {
  min-width: 106px;
  font-weight: var(--weight-medium);
}

.login-provider-blurb {
  color: var(--brand-muted);
  font-size: var(--text-xs);
}

h1 {
  margin: 0 0 var(--space-1);
  font-size: var(--text-xl);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tighter);
}

.login-lede {
  margin: 0 0 var(--space-6);
  color: var(--muted);
  font-size: var(--text-base);
}

/* Google Sign-In branding: white surface, #dadce0 border, four-color mark.
   These are Google's prescribed values, not this app's palette — the one place
   besides the brand panel where a literal color is correct. */
.btn-google {
  --g-bg: #ffffff;
  --g-border: #dadce0;
  --g-text: #1f1f1f;
  --g-hover: #f7f8f8;

  width: 100%;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-3);
  padding: var(--space-3) var(--space-4);
  border: 1px solid var(--g-border);
  border-radius: var(--radius-sm);
  background: var(--g-bg);
  color: var(--g-text);
  font-size: var(--text-base);
  font-weight: var(--weight-medium);
  cursor: pointer;
  transition:
    background var(--duration-fast) var(--ease),
    box-shadow var(--duration-fast) var(--ease);
}

.btn-google:hover {
  background: var(--g-hover);
  box-shadow: var(--shadow);
}

/* 18px is the mark's own size in Google's spec, like any other icon dimension. */
.btn-google-mark {
  width: 18px;
  height: 18px;
  flex-shrink: 0;
}

.login-error {
  margin-top: var(--space-4);
}

@media (prefers-color-scheme: dark) {
  /* Google's dark-theme button variant. */
  .btn-google {
    --g-bg: #131314;
    --g-border: #8e918f;
    --g-text: #e3e3e3;
    --g-hover: #1e1f20;
  }

  .btn-google:hover {
    box-shadow: none;
  }
}

/* ---- Responsive ---- */

@media (max-width: 860px) {
  .login-providers {
    padding-top: var(--space-4);
  }
}

@media (max-width: 480px) {
  .login-providers li {
    flex-direction: column;
    gap: 0;
  }

  .login-provider-name {
    min-width: 0;
  }
}
</style>
