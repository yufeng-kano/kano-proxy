<script setup lang="ts">
/**
 * The one surface a signed-out visitor sees.
 *
 * Two columns: a brand panel that says what this is, and the sign-in panel.
 * The brand panel is deliberately dark in **both** themes — the single
 * documented exception to the token palette (docs/admin-ui.md § Theming), so it
 * carries its own local `--brand-*` values rather than `--surface`.
 *
 * The Google button is the other prescribed surface: white background,
 * `#dadce0` border, the official four-color mark inlined as SVG (never a remote
 * asset), and Google's own dark variant. Those values are Google's to set, not
 * ours to tokenize.
 */
import { useRoute } from "vue-router"
import { useAuth } from "@/composables/useAuth"
import Banner from "@/components/ui/Banner.vue"
import { SITE } from "@/config/site"
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

/**
 * Computed at render so it never goes stale. A string, not a number: the
 * interpolator runs numbers through `Intl.NumberFormat`, which would print the
 * year with a thousands separator.
 */
const year = String(new Date().getFullYear())
</script>

<template>
  <div class="login-page">
    <aside class="login-brand">
      <div class="login-brand-inner">
        <div class="login-logo">
          <span class="login-mark" aria-hidden="true">k</span>
          <span class="login-wordmark">{{ SITE.name }}</span>
        </div>

        <div class="login-pitch">
          <h2>{{ t("login.pitch.title") }}</h2>
          <p>{{ t("login.pitch.body") }}</p>
        </div>

        <ul class="login-providers">
          <li v-for="p in PROVIDERS" :key="p.id">
            <span class="login-provider-name">{{ t(NAME_KEY[p.id]) }}</span>
            <span class="login-provider-blurb">{{ t(BLURB_KEY[p.id]) }}</span>
          </li>
        </ul>
      </div>
    </aside>

    <main class="login-panel">
      <div class="login-form">
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
      </div>

      <footer class="login-footer">
        <span>{{ t("login.copyright", { year, name: SITE.name }) }}</span>
        <span class="login-footer-links">
          <!-- Plain anchor, not RouterLink: /docs/ is the static docs site, outside the router. -->
          <a class="login-contact" href="/docs/">{{ t("nav.docs") }}</a>
          <a
            v-if="SITE.contactEmail"
            class="login-contact"
            :href="`mailto:${SITE.contactEmail}`"
          >
            {{ SITE.contactEmail }}
          </a>
        </span>
      </footer>
    </main>
  </div>
</template>

<!-- Global: the login route is full-bleed, so the document background must
     match the sign-in panel rather than the app's --bg. -->
<style>
html:has(.login-page),
body:has(.login-page) {
  background: var(--surface);
}
</style>

<style scoped>
/*
 * The document itself no longer scrolls (`html, body { height: 100%; overflow:
 * hidden }` in styles.css), so this page is its own scroll container: it fills
 * the frame exactly and takes the overflow itself when a short window — a
 * laptop at 600px tall, a phone in landscape — can't hold the panel.
 *
 * `100%` of the locked chain (html → body → #app, all 100%) rather than
 * `100dvh`: where mobile browser chrome makes the two disagree, a dvh taller
 * than the body would put the footer under `body`'s `overflow: hidden` with no
 * scroll that can reach it.
 */
.login-page {
  height: 100%;
  overflow-y: auto;
  display: grid;
  grid-template-columns: minmax(0, 1.05fr) minmax(0, 1fr);
}

/* ---- Brand panel (intentionally dark in both themes) ---- */

.login-brand {
  --brand-bg: #0b0b0f;
  --brand-text: #fafafa;
  --brand-muted: #a1a1aa;
  --brand-line: rgb(255 255 255 / 5%);

  position: relative;
  overflow: hidden;
  display: flex;
  align-items: center;
  padding: var(--space-12);
  color: var(--brand-text);
  background:
    radial-gradient(
      ellipse 110% 75% at 10% -10%,
      rgb(255 255 255 / 7%) 0%,
      transparent 62%
    ),
    radial-gradient(
      ellipse 80% 60% at 95% 110%,
      rgb(255 255 255 / 4%) 0%,
      transparent 58%
    ),
    var(--brand-bg);
}

/* Fine grid texture. The mask fades it out well before the panel edges so the
   texture never terminates on a visible line. */
.login-brand::before {
  content: "";
  position: absolute;
  inset: -10%;
  background-image:
    linear-gradient(var(--brand-line) 1px, transparent 1px),
    linear-gradient(90deg, var(--brand-line) 1px, transparent 1px);
  background-size: 56px 56px;
  mask-image: radial-gradient(
    ellipse 95% 85% at 12% 6%,
    #000 0%,
    rgb(0 0 0 / 55%) 42%,
    transparent 78%
  );
  pointer-events: none;
}

/* A column measure, not spacing — same class as CopyField's label track. */
.login-brand-inner {
  position: relative;
  display: grid;
  gap: var(--space-10);
  width: min(460px, 100%);
  margin-left: auto;
}

.login-logo {
  display: flex;
  align-items: center;
  gap: var(--space-3);
}

.login-mark {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: var(--space-10);
  height: var(--space-10);
  border-radius: var(--radius);
  background: var(--brand-text);
  color: var(--brand-bg);
  font-size: var(--text-lg);
  font-weight: var(--weight-bold);
  letter-spacing: var(--tracking-tighter);
}

.login-wordmark {
  font-size: var(--text-md);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
}

.login-pitch h2 {
  margin: 0 0 var(--space-3);
  font-size: var(--text-2xl);
  line-height: 1.18;
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tighter);
  text-wrap: balance;
}

.login-pitch p {
  margin: 0;
  max-width: 42ch;
  font-size: var(--text-base);
  line-height: 1.65;
  color: var(--brand-muted);
}

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

/* ---- Sign-in panel ---- */

.login-panel {
  /* Sign-in block centers in the space above the footer, which stays pinned
     to the bottom edge like a normal site footer. */
  display: grid;
  grid-template-rows: 1fr auto;
  padding: var(--space-12) var(--space-12) var(--space-8);
  background: var(--surface);
}

.login-form {
  width: min(360px, 100%);
  align-self: center;
}

.login-form h1 {
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

.login-footer {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2) var(--space-5);
  padding-top: var(--space-5);
  border-top: 1px solid var(--border);
  /* --muted, not --faint: --faint only reaches 2.56:1 on --surface in light
     mode, below the WCAG AA floor for copy this small. */
  color: var(--muted);
  font-size: var(--text-xs);
}

.login-footer-links {
  display: flex;
  gap: var(--space-4);
}

.login-contact {
  color: var(--muted);
  transition: color var(--duration-fast) var(--ease);
}

.login-contact:hover {
  color: var(--text);
  text-decoration: underline;
  text-underline-offset: 3px;
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

/* Two columns still fit, but the full gutters start crowding the copy. */
@media (max-width: 1080px) {
  .login-brand {
    padding: var(--space-10);
  }

  .login-panel {
    padding: var(--space-10) var(--space-10) var(--space-8);
  }

  .login-pitch h2 {
    font-size: var(--text-xl);
  }
}

@media (max-width: 860px) {
  .login-page {
    /* Brand strip sizes to content; the sign-in panel absorbs the rest so its
       surface reaches the bottom of the viewport. */
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: auto 1fr;
  }

  .login-brand {
    padding: var(--space-8) var(--space-6);
  }

  .login-brand::before {
    background-size: 40px 40px;
  }

  .login-brand-inner {
    width: 100%;
    margin-left: 0;
    gap: var(--space-6);
  }

  .login-providers {
    padding-top: var(--space-4);
  }

  /* Single column: form sits at the top of the panel, footer keeps its own
     width so its rule lines up with the sign-in block above it. */
  .login-panel {
    justify-items: center;
    padding: var(--space-10) var(--space-6) var(--space-8);
  }

  .login-form {
    align-self: start;
  }

  .login-footer {
    width: min(360px, 100%);
  }
}

@media (max-width: 560px) {
  .login-pitch h2 {
    font-size: var(--text-lg);
  }

  .login-brand {
    padding: var(--space-6) var(--space-4);
  }

  /* Bottom pad clears the home indicator: `viewport-fit=cover` runs the page
     under it, and the footer is the last thing in this panel. */
  .login-panel {
    padding: var(--space-8) var(--space-4)
      calc(var(--space-6) + env(safe-area-inset-bottom, 0px));
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
