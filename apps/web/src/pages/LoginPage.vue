<script setup lang="ts">
import { useAuth } from "@/composables/useAuth"
import { SITE } from "@/config/site"
import { PROVIDERS } from "@/types"

const { goLogin, error } = useAuth()
const year = new Date().getFullYear()
</script>

<template>
  <div class="login-page">
    <aside class="login-brand">
      <div class="login-brand-inner">
        <div class="login-logo">
          <span class="login-mark">k</span>
          <span class="login-wordmark">{{ SITE.name }}</span>
        </div>

        <div class="login-pitch">
          <h2>One base URL for every coding agent.</h2>
          <p>
            Bind your Claude Code, Codex, and Grok subscriptions to a private
            pool, then point any OpenAI- or Anthropic-compatible client at a
            single endpoint.
          </p>
        </div>

        <ul class="login-providers">
          <li v-for="p in PROVIDERS" :key="p.id">
            <span class="login-provider-name">{{ p.name }}</span>
            <span class="login-provider-blurb">{{ p.blurb }}</span>
          </li>
        </ul>
      </div>
    </aside>

    <main class="login-panel">
      <div class="login-form">
        <h1>Sign in</h1>
        <p class="login-lede">Continue to your account pool.</p>

        <button type="button" class="btn-google" @click="goLogin">
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
          <span>Continue with Google</span>
        </button>

        <p v-if="error" class="banner error login-error">{{ error }}</p>
      </div>

      <footer class="login-footer">
        <span>© {{ year }} {{ SITE.name }}</span>
        <a
          v-if="SITE.contactEmail"
          class="login-contact"
          :href="`mailto:${SITE.contactEmail}`"
        >
          {{ SITE.contactEmail }}
        </a>
      </footer>
    </main>
  </div>
</template>

<!-- Global: the login route is full-bleed, so the page background must match the
     sign-in panel. Otherwise the reserved scrollbar gutter (`scrollbar-gutter:
     stable` in styles.css) shows a stripe of --bg down the right edge. -->
<style>
html:has(.login-page),
body:has(.login-page) {
  background: var(--surface);
}
</style>

<style scoped>
.login-page {
  min-height: 100vh;
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
  padding: 56px 56px 56px 64px;
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

.login-brand-inner {
  position: relative;
  display: grid;
  gap: 40px;
  width: min(460px, 100%);
  margin-left: auto;
}

.login-logo {
  display: flex;
  align-items: center;
  gap: 12px;
}

.login-mark {
  width: 40px;
  height: 40px;
  border-radius: 10px;
  background: var(--brand-text);
  color: var(--brand-bg);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  font-size: 20px;
  font-weight: 700;
  letter-spacing: -0.04em;
}

.login-wordmark {
  font-size: 16px;
  font-weight: 600;
  letter-spacing: -0.02em;
}

.login-pitch h2 {
  margin: 0 0 14px;
  font-size: 32px;
  line-height: 1.18;
  font-weight: 600;
  letter-spacing: -0.035em;
  text-wrap: balance;
}

.login-pitch p {
  margin: 0;
  max-width: 42ch;
  font-size: 14px;
  line-height: 1.65;
  color: var(--brand-muted);
}

.login-providers {
  display: grid;
  gap: 1px;
  margin: 0;
  padding: 20px 0 0;
  list-style: none;
  border-top: 1px solid var(--brand-line);
}

.login-providers li {
  display: flex;
  align-items: baseline;
  gap: 10px;
  padding: 7px 0;
  font-size: 13px;
}

.login-provider-name {
  min-width: 106px;
  font-weight: 550;
}

.login-provider-blurb {
  color: var(--brand-muted);
  font-size: 12.5px;
}

/* ---- Sign-in panel ---- */

.login-panel {
  /* Sign-in block centers in the space above the footer, which stays pinned
     to the bottom edge like a normal site footer. */
  display: grid;
  grid-template-rows: 1fr auto;
  padding: 48px 64px 28px;
  background: var(--surface);
}

.login-form {
  width: min(360px, 100%);
  align-self: center;
}

.login-form h1 {
  margin: 0 0 6px;
  font-size: 26px;
  font-weight: 600;
  letter-spacing: -0.035em;
}

.login-lede {
  margin: 0 0 28px;
  color: var(--muted);
  font-size: 14px;
}

/* Google Sign-In branding: white surface, #dadce0 border, four-color mark. */
.btn-google {
  --g-bg: #ffffff;
  --g-border: #dadce0;
  --g-text: #1f1f1f;
  --g-hover: #f7f8f8;

  width: 100%;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 12px;
  padding: 12px 16px;
  border: 1px solid var(--g-border);
  border-radius: var(--radius-sm);
  background: var(--g-bg);
  color: var(--g-text);
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  transition: background 0.12s ease, box-shadow 0.12s ease;
}

.btn-google:hover {
  background: var(--g-hover);
  box-shadow: 0 1px 3px rgb(0 0 0 / 12%);
}

.btn-google:focus-visible {
  outline: 2px solid #4285f4;
  outline-offset: 2px;
}

.btn-google-mark {
  width: 18px;
  height: 18px;
  flex-shrink: 0;
}

.login-error {
  margin: 16px 0 0;
}

.login-footer {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 6px 20px;
  padding-top: 18px;
  border-top: 1px solid var(--border);
  /* --muted, not --faint: --faint only reaches 2.56:1 on --surface in light
     mode, below the WCAG AA floor for this 12.5px copy. */
  color: var(--muted);
  font-size: 12.5px;
}

.login-contact {
  color: var(--muted);
  transition: color 0.12s ease;
}

.login-contact:hover {
  color: var(--text);
  text-decoration: underline;
  text-underline-offset: 3px;
}

.login-contact:focus-visible {
  outline: 2px solid var(--ring-border);
  outline-offset: 3px;
  border-radius: 2px;
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

/* Two columns still fit, but 64px gutters start crowding the copy. */
@media (max-width: 1080px) {
  .login-brand {
    padding: 48px 36px 48px 40px;
  }

  .login-panel {
    padding: 40px 40px 28px;
  }

  .login-pitch h2 {
    font-size: 27px;
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
    padding: 32px 24px;
  }

  .login-brand::before {
    background-size: 40px 40px;
  }

  .login-brand-inner {
    width: 100%;
    margin-left: 0;
    gap: 24px;
  }

  .login-pitch h2 {
    font-size: 24px;
  }

  .login-providers {
    padding-top: 16px;
  }

  /* Single column: form sits at the top of the panel, footer keeps its own
     width so its rule lines up with the sign-in block above it. */
  .login-panel {
    justify-items: center;
    padding: 44px 24px 28px;
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
    font-size: 21px;
  }
}

@media (max-width: 480px) {
  .login-providers li {
    flex-direction: column;
    gap: 1px;
  }

  .login-provider-name {
    min-width: 0;
  }
}
</style>
