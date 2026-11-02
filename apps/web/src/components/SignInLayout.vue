<script setup lang="ts">
/**
 * The two-column surface a visitor meets before the app: the sign-in page and
 * the usage notice page (docs/admin-ui.md § Login page, § Usage notice page).
 *
 * A brand panel that says what this is, and a panel with the page's own
 * content above the site footer. The brand panel is deliberately dark in
 * **both** themes, the single documented exception to the token palette
 * (docs/admin-ui.md § Theming), so it carries its own local `--brand-*` values
 * rather than `--surface`. The `brand` slot sits under the pitch; the default
 * slot is the panel's content.
 */
import { SITE } from "@/config/site"
import { useWebExtensions } from "@/extensions"
import { useI18n } from "@/i18n"

withDefaults(
  defineProps<{
    /** A wider column for prose (the notice) than for a single button (sign-in). */
    wide?: boolean
  }>(),
  { wide: false },
)

const { t } = useI18n()
/** The edition's landing page and footer links (docs/cloud-edition.md § Web shell); the standalone build has neither. */
const { landing, loginLinks = [] } = useWebExtensions()

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
        <RouterLink v-if="landing" :to="landing" class="login-logo">
          <span class="login-mark" aria-hidden="true">k</span>
          <span class="login-wordmark">{{ SITE.name }}</span>
        </RouterLink>
        <div v-else class="login-logo">
          <span class="login-mark" aria-hidden="true">k</span>
          <span class="login-wordmark">{{ SITE.name }}</span>
        </div>

        <div class="login-pitch">
          <h2>{{ t("login.pitch.title") }}</h2>
          <p>{{ t("login.pitch.body") }}</p>
        </div>

        <slot name="brand" />
      </div>
    </aside>

    <main class="login-panel" :class="{ wide }">
      <div class="login-content">
        <slot />
      </div>

      <footer class="login-footer">
        <span>{{ t("login.copyright", { year, name: SITE.name }) }}</span>
        <span class="login-footer-links">
          <!-- Plain anchor, not RouterLink: /docs/ is the static docs site, outside the router. -->
          <a class="login-contact" href="/docs/">{{ t("nav.docs") }}</a>
          <a v-for="link in loginLinks" :key="link.href" class="login-contact" :href="link.href">
            {{ link.label }}
          </a>
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

<!-- Global: the sign-in surface is full-bleed, so the document background must
     match its panel rather than the app's --bg. -->
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
  /* A link when the edition has a landing page; it keeps the panel's text colour. */
  color: inherit;
  text-decoration: none;
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

/* ---- Content panel ---- */

.login-panel {
  /* The page's content centers in the space above the footer, which stays
     pinned to the bottom edge like a normal site footer. Column measures, not
     spacing: one button needs less width than a page of prose. */
  --login-measure: min(360px, 100%);

  display: grid;
  grid-template-rows: 1fr auto;
  /* Keeps a tall page's last line off the footer rule when the panel scrolls. */
  row-gap: var(--space-8);
  padding: var(--space-12) var(--space-12) var(--space-8);
  background: var(--surface);
}

.login-panel.wide {
  --login-measure: min(440px, 100%);
}

.login-content {
  width: var(--login-measure);
  align-self: center;
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
  flex-wrap: wrap;
  gap: var(--space-1) var(--space-4);
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
    /* Brand strip sizes to content; the content panel absorbs the rest so its
       surface reaches the bottom of the viewport. `max-content`, not `auto`:
       the strip clips its texture with `overflow: hidden`, which lets an `auto`
       row shrink below its content when a tall page (the notice) needs the
       room, cutting the strip's top off. */
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: max-content 1fr;
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

  /* Single column: content sits at the top of the panel, footer keeps its
     own width so its rule lines up with the content above it. */
  .login-panel {
    justify-items: center;
    padding: var(--space-10) var(--space-6) var(--space-8);
  }

  .login-content {
    align-self: start;
  }

  .login-footer {
    width: var(--login-measure);
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
</style>
