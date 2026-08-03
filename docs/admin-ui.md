# Web UI

Vue 3 + Vite + TypeScript on Cloudflare Pages (same hostname as API via routes). All copy comes from the message catalog — see [i18n.md](./i18n.md).

## Pages

| Route | Content |
|-------|---------|
| `/` | Redirect to overview or login |
| `/login` | Google sign-in (split brand panel + sign-in panel) |
| `/overview` | Usage and cache-rate dashboard over `request_logs` |
| `/providers` | Connected subscription accounts (Claude / Codex / Grok) and custom endpoints, one section per provider |
| `/models` | Searchable catalog of `provider/model` ids, grouped by provider |
| `/keys` | Create / revoke API keys, plus the client connection details |
| `/changelog` | Published GitHub Releases, newest first ([changelog.md](./changelog.md)) |

`/dashboard` and `/accounts` are kept as permanent redirects to `/overview` and `/providers` — a bookmark or a persisted last-route from an older build must not 404 into the catch-all.

## Layout: the shell

The signed-in app is a **fixed frame, not a scrolling document**. `AppShell.vue` owns a `100dvh` grid; only the content region scrolls, so the sidebar, the page header, and any section nav stay put no matter how long a table gets.

```text
┌──────────┬─────────────────────────────────┐
│ sidebar  │ page header (sticky)            │  ← title, actions, section nav
│ (fixed)  ├─────────────────────────────────┤
│          │ content region (scrolls)        │
│  brand   │                                 │
│  nav     │                                 │
│  ────    │                                 │
│  user    │                                 │
└──────────┴─────────────────────────────────┘
```

- **Sidebar** 248px, its own column, never scrolls with content: brand at the top, primary nav below it, then the Changelog link carrying the running version pinned to the bottom of the nav area, and the signed-in user with sign-out in the footer beneath.
- **Active nav item** is marked by a filled pill **and** a step up in font weight — never color alone. The fill on its own is a ~2% luminance delta; the weight is what makes it read.
- **Page header** is sticky at the top of the content region and holds the page title, its primary actions, and (where a page has sections) the section nav. It is the same component on every page.
- The content region is the scroll container, so `window.scrollY` is meaningless here; scroll restore listens on that element (see [View preferences](#view-preferences-localstorage)).
- `AppShell` publishes its own metrics as inherited custom properties — `--page-top`, `--page-bottom`, `--page-gutter`, and `--page-chrome` (the shell chrome above the region: 0 on desktop, the mobile bar below the shell breakpoint). A page that sizes itself to the viewport, or a header that cancels the gutter to bleed its blur, **reads those** rather than restating the values. A second copy drifts the first time only one of them changes at a breakpoint.

### Anti-scroll rules

The core constraint: **the user should not have to scroll to find things**. Concretely:

- Every page's primary controls live in the sticky header, reachable at any scroll depth.
- Long collections get **in-page navigation instead of stacking**: Providers uses a section nav that scrolls the matching section into view within the content region; Models filters by provider group; Keys splits its two jobs into tabs.
- Wide/tall data goes in bounded regions with sticky column headers, not an unbounded page. A table scrolls inside its own card.
- The Overview page targets a single viewport at 1440×900: stat row, then chart and per-model breakdown side by side. Only the breakdown scrolls internally.
- New sections are added to a section nav, never appended to the bottom of an already-long page.

## Responsive

Four breakpoints, and no more: `640` (phone), `768` (tables), `1080` (shell), `1200` (content gutter).

| Width | Shell |
|-------|-------|
| ≥ 1080px | Full sidebar (248px) + content |
| < 1080px | Sidebar becomes a slide-in drawer behind a header menu button |

No icon rail and no bottom tab bar. With five destinations the text labels are what make the nav scannable, so a rail would trade legibility for width the content does not need; a bottom bar would cost 56px of the scarcest axis on a phone and read as a mobile app rather than a web app.

- Tables below 768px render as stacked cards (label + value rows), not horizontal scroll — a horizontally scrolling table on a phone hides the columns that matter. `DataTable` does this; a hand-rolled table does not.
- Stat tiles reflow on **container** width, not viewport width: the sidebar's presence changes the content width, and a viewport media query gets that wrong at every shell state. The hero (cache rate) spans two columns until the layout drops to two.
- The chart's plot area is a **fixed height**, never an aspect ratio — an aspect-ratio chart in a wide content region eats the viewport. Bucket-label density thins instead; labels never rotate.
- Dialogs become bottom sheets below 640px, with `env(safe-area-inset-bottom)` clearance.
- Touch targets are ≥ 40px on coarse pointers, and inputs go to 16px there so iOS Safari does not zoom on focus.

## Theming

- All colors live as CSS custom properties on `:root` in `apps/web/src/styles.css`. Components and pages reference tokens only — no hardcoded hex/rgb outside that token block. **The palette is fixed**: the token *values* are the product's identity and are not changed by a UI refactor.
- Light and dark are both first-class: dark values are declared in a single `@media (prefers-color-scheme: dark)` override of the same token names. `:root` sets `color-scheme: light dark` so native controls, scrollbars, and form widgets follow the OS.
- No in-app theme toggle — the UI follows the OS preference. Adding a manual toggle requires a docs update first.
- `--accent` / `--accent-fg` invert between themes (near-black button on light, near-white on dark). Anything using `--accent` must stay legible after that flip.
- Dashboard chart colors are two separate token families: `--chart-input-soft` / `--chart-input` / `--chart-completion` encode **token kind** (uncached / cached / completion), and `--series-1..6` + `--series-other` encode **model identity** in the grouped and curve views. Both are validated categorical sets, not hand-picked — see the note above each block in `styles.css` before changing a value.
- The login brand panel is intentionally dark in **both** themes; it uses its own local values, not `--surface`.

### Scales

Spacing, radius, type, and motion are tokens too (`--space-*`, `--radius-*`, `--text-*`, `--weight-*`, `--duration-*`, `--ease*`), so density is tuned in one place instead of drifting per component. Use the scale; a raw `padding: 13px` in a component is the same class of bug as a raw hex. Structural track widths (a grid column sized to its label, a flex-wrap threshold) are the exception — those are layout arithmetic, not spacing.

Two conventions worth stating because they are easy to violate accidentally:

- **Weight carries as much signal as color.** The active nav item is a filled pill *and* a step up in font weight; the fill alone is a ~2% luminance delta and reads as noise without it.
- **Never transition `all`** — name the properties. Hover and press use `--duration-fast`; a panel entering uses `--ease-enter` at `--duration-slow` and leaving uses `--ease-exit` at `--duration`, asymmetric on purpose: an entrance should be seen, an exit should get out of the way.

## Component primitives

`apps/web/src/components/ui/` holds the shared vocabulary. Pages compose these rather than restyling their own variants:

`AppShell`, `PageHeader`, `SectionNav`, `Segmented`, `StatTile`, `DataTable`, `Card`, `Modal`, `EmptyState`, `Banner`, `StatusDot`, `CopyField`, `UsageBar`, `SkeletonBlock`, `Spinner`.

`DataTable` is the one place table markup lives: it owns the sticky header, the tabular-numeral alignment, and the mobile card fallback. A page that hand-rolls a `<table>` will not get those.

Two implementation notes that are not obvious and cost real debugging time:

- A sticky `<th>` must draw its bottom rule with `box-shadow: inset 0 -1px 0` — a `border-bottom` on a sticky cell scrolls away independently of the cell, leaving the header floating unruled.
- A component that both exports a type and uses `<script setup>` needs a **plain** `<script lang="ts">` block for the export. A second `<script setup>` block is silently dropped along with every macro in it, and the component renders nothing.

## Data freshness

Cache-first everywhere, and **invisible to the user** — no "cached" / "refreshing" / "last updated" text on any surface. Freshness is the app's job.

- Paint the `localStorage` cache immediately (even if stale); fetch only if the cache is older than **90s**, or on an explicit user Refresh.
- Server-data caches live in **`localStorage`**, not sessionStorage, so a reopened tab or browser restart paints the last known data instantly and multiple tabs share one cache. Each entry is a versioned envelope `{ v, savedAt, data }`; a bumped `CACHE_SCHEMA_VERSION` or malformed blob reads as a miss, never as trusted data.
- Providers page polls every **90s**; Refresh sets `?refresh=true` (bypasses the server-side models KV cache; usage is always fetched live server-side).
- Server-side KV caching is deliberately minimal (KV free-tier writes are the scarce resource): **per-account usage has no KV layer** — `GET /api/providers/{provider}/accounts` fetches upstream usage live on every call, and upstream-429 protection comes from the frontend's 90s localStorage TTL + 90s poll being the only callers. The **models catalog keeps its KV cache at a 1h TTL** because the client-facing `GET /openai/v1/models` / `GET /anthropic/v1/models` are hit by API clients with no frontend cache (see [providers.md](./providers.md)).
- On refresh failure, keep showing cache and surface a non-blocking error.
- A background refresh shows no spinner. A *user-initiated* refresh shows progress on the control they pressed — that one is their action, so it gets feedback.
- First load with no cache shows skeletons shaped like the content, never a bare "Loading…".
- Never store access tokens, refresh tokens, session secrets, or a custom provider's API key in local UI cache — a cached custom-provider row carries only non-secret fields the `GET /api/custom-providers` response already returns (`key_mask`, never the key). What *is* cached (account emails/labels, usage percentages, model ids, key masks) is non-secret display data, cleared by the logout sweep.
- Cache keys are scoped to the signed-in user id: `kano-proxy:custom-providers:{userId}`, `kano-proxy:usage:{userId}:{days}`, etc.
- **Changelog is the one exception**: TTL is **1 hour** and the key `kano-proxy:changelog` carries **no user id** — the data is identical for every operator and holds nothing user-identifying. The logout sweep still clears it. See [changelog.md](./changelog.md).

### View preferences (`localStorage`)

Server **data** caches (above) and **view preferences** — what the user picked, not what the server said — both persist in `localStorage`, but stay separate modules: data entries are user-id-scoped versioned envelopes swept on logout, while preferences live under a single `kano-proxy:prefs` key that survives logout so a reopened tab lands where the user left off.

- Stored: last visited route path, per-route scroll offset **of the content region**, the Overview range (24h/7d/30d) and chart view (tokens / cache-rate), the chart-vs-table toggle, and the Models page's active provider filter.
- **Never** stores tokens, session state, emails, or any server payload — only enum-ish UI choices and integers. Unlike the server-data caches it is therefore **not** user-id scoped and **not** swept on logout: it holds nothing user-identifying, and a shared machine reveals only a route name.
- Every read is validated against the current allowed values and falls back to the default on anything unexpected (stale schema, hand-edited storage, removed route). A malformed blob is discarded, never trusted.
- Restore-on-boot only replays a route the router still knows and the signed-in user may see; the auth guard runs unchanged, so a persisted path never bypasses login.
- Scroll restore waits for the page's first data paint, then sets the offset on the content region once; a user scroll during restore cancels it rather than fighting the user.

## Overview page

Route `/overview`; signed-in `/` redirects here. Data source: `GET /api/usage/summary?days=1|7|30` (session auth, see [auth.md](./auth.md)) aggregating `request_logs` — **live rows only, no fabricated numbers**; an empty range renders an explicit empty state with a link to Keys.

- Range picker (24h / 7d / 30d, `days=1|7|30`, default 7) lives in the sticky page header. Series buckets are hourly for 24h, daily otherwise (UTC bucket keys from the API, formatted client-side; missing buckets zero-filled client-side). Time runs **oldest-left → newest-right**.
- Stat tiles: requests, tokens (input + output), **cache hit rate** (hero: Σ`cache_read_input_tokens` / Σ`prompt_tokens` over cache-known rows), errors, avg latency.
- Below the tiles, a two-column region at ≥ 1200px: the time-series card and the per-model breakdown side by side, so neither pushes the other off-screen. Single column below that.
- Per-model breakdown: requests, input/output tokens, cache read/write, cache rate; sorted by total tokens desc, scrolling inside its own card with a sticky header. When cache data covers only part of a model's requests, the coverage is annotated instead of silently mixed.
- **Chart switcher** — two views over the same range, one visible at a time:
  - **Tokens** (default): **grouped** columns — within each bucket, one column per model, ordered by the model's range-total tokens desc (same order as the breakdown). A column is one solid fill in the model's own color: hue does **identity**, so the uncached / cached / completion split moves to the tooltip and the table rather than being double-encoded. Models past the categorical cap fold into a single **Other** column rather than growing the palette.
  - **Cache rate**: one 2px line per model of that bucket's cache hit rate (Σ`cache_read_input_tokens` / Σ`prompt_tokens` within the bucket), on a fixed 0–100% y-axis. Buckets with no cache-known request are gaps in the line, not zeros — an unreported bucket is not a 0% bucket.
- Both views are hand-rolled inline SVG — **no charting dependency**; colors/typography from `styles.css` tokens only, legible in both themes. Model identity uses `--series-1..6` + `--series-other`, assigned by the model's rank in the range totals so a model keeps its color across buckets. Every view ships a legend, a hover/focus tooltip, and a **table** twin so identity and values are never color-only.
- Requests without usage data (`NULL` token fields — see [database.md](./database.md)) count toward request/error totals but are skipped by token/cache aggregates; the UI surfaces that coverage rather than hiding it.

### Series shape

`series[]` carries a per-model dimension so the grouped and per-model-curve views need no second request. Each point is one `(bucket, provider, model)` group: `bucket`, `provider`, `model`, `requests`, `prompt_tokens`, `completion_tokens`, `cache_read_input_tokens`, `cache_known_requests`. Sparse — only groups with at least one row; the client zero-fills the bucket grid and folds the model tail into "Other". Bucket-level totals are the client-side sum over a bucket's model points, not a separate field.

## Providers page

One section per provider plus a custom-endpoints section, reachable from a section nav in the sticky header that shows each provider's connected-account count. The nav scrolls the section into view inside the content region rather than stacking everything for the user to hunt through.

Subscription accounts (Claude / Codex / Grok):

- Status dot: active / standby / benched / unusable, always paired with a text label — never color alone.
- Progress bars per usage window (5h, Week, …). `utilization` is always a **percent (0–100)**, never a 0–1 fraction — adapters normalize upstream values to that scale, so the bar renders it directly (clamped and rounded) with no rescaling heuristic.
- Make primary / remove. Add account → provider-specific sign-in flow in a dialog.

Custom endpoints (`GET /api/custom-providers` — see [auth.md](./auth.md)):

- Name, a format badge (`OpenAI` | `Anthropic`), the `slug/*` model-prefix hint, the base URL, the key mask (e.g. `sk-abc…f3a2`), and a status dot (**active** / **benched** only — a static key has no usage window to show).
- Row actions: **Test** (`POST /api/custom-providers/test` with `{id}`, inline result), **Edit**, **Remove** (confirms first since it also deletes the stored key).
- **Add endpoint** dialog: format toggle (immutable once saved); name with a slug auto-generated from it (editable before first save, then locked — immutable server-side too); base URL with a **live preview of the resolved endpoint** as the user types (`{base}/chat/completions` for OpenAI, `{base}/v1/messages` for Anthropic, matching the literal-concatenation rule in [providers.md](./providers.md)); API key as `type="password"`, **never pre-filled or echoed** — on edit, blank means "keep the existing key" (matches the backend's blank-means-keep contract, see [auth.md](./auth.md)); models mode toggle (auto / manual, with a textarea for manual ids); a **Test connection** button that calls the same endpoint with the in-progress form values and renders the result inline without blocking Save.

## Models page

- Sources (only providers with a bound usable account are queried):
  - Claude Code: live upstream `GET /v1/models` with OAuth
  - Grok: live upstream `GET /v1/models` with OAuth
  - Codex: no public / third-party models list → empty state pointing at the user's plan (see [providers.md](./providers.md))
  - Custom providers: one group per user-defined endpoint, from the same `GET /api/models` payload; manual list or live-with-fallback depending on that provider's `models_mode`
- Provider groups are **dynamic**, rendered from whatever `providers` the response lists, so a new custom endpoint appears without a UI code change.
- A search box filters across every group by model id and display name; a provider filter in the header narrows to one group. Both are client-side over already-loaded data — no request per keystroke. The active filter persists as a view preference.
- Session API: `GET /api/models` (`?refresh=true` bypasses the 1h server KV cache). Client-facing `GET /openai/v1/models` and `GET /anthropic/v1/models` return the same live catalog; ids are always `provider/upstream`.
- Copy model id as `provider/upstream` — works on **both** OpenAI and Anthropic bases.

## Keys page

Two tabs in the sticky header: **Keys** (create / revoke, with the plaintext shown once at creation) and **Connect** (the client setup details).

Connect shows the base URLs derived from the current deploy host (`VITE_API_ORIGIN` locally, same-origin in production) — not hard-coded:

```text
OpenAI-compatible:     https://<your-domain>/openai/v1
Anthropic-compatible:  https://<your-domain>/anthropic
```

…plus how to send the key and the `provider/model` id form. Each value is a copy field.

## Login page

- Two-column split: dark brand panel (left) + sign-in panel (right). Collapses to a single column below **860px**, where the brand panel becomes a compact header strip.
- Brand panel content: enlarged `k` mark, product pitch, and the provider list sourced from the shared `PROVIDERS` constant in `apps/web/src/types/index.ts` (single source of truth with Providers/Models — do not retype provider names; their display names and blurbs come from the message catalog).
- Sign-in button follows Google Sign-In branding: white surface, `#dadce0` border, official four-color G mark inlined as SVG (no CDN/remote asset). Dark theme uses Google's dark variant (`#131314` surface, `#8e918f` border).
- Auth errors render in-panel via the shared banner style.
- Site footer pinned to the bottom of the sign-in panel: `© <year> <site name>` on the left, contact `mailto:` link on the right. The year is computed at render so it never goes stale.
- Footer values come from `apps/web/src/config/site.ts`. The contact address is deploy-specific and read from **`VITE_CONTACT_EMAIL`**. There is **no fallback address** — when unset, `contactEmail` is empty and the link is not rendered, so a stale default can never ship.
- `SITE.name` is the display brand for every user-facing surface: login wordmark, footer copyright, and the signed-in sidebar. `index.html` repeats it in `<title>` because the static shell renders before the app boots — keep those in sync when renaming. The `sk-kano-proxy-` API key prefix and the `kano-proxy:*` browser-storage keys are wire/storage identifiers and are **not** renamed with the brand.
- Login-specific CSS lives in `LoginPage.vue` `<style scoped>`, not in `styles.css`.

## Accessibility floor

- Every interactive element is reachable and operable by keyboard, with a visible focus ring (`--ring-border`, ≥ 3:1 on its surface per WCAG 1.4.11).
- Status is never color-only: a dot always has a text label, and every chart ships a legend, tooltip, and table twin.
- Dialogs trap focus, close on `Esc`, and return focus to the control that opened them. The mobile nav drawer is modal too (it covers the page behind a scrim), so it owes the same three — and, being off-screen rather than unmounted when closed, it must also be genuinely inert so `Tab` cannot walk into an invisible menu.
- Skip-to-content link before the sidebar; the content region is a labelled `<main>`.
- Body text clears WCAG AA on its own surface — see the secondary-text ramp note in `styles.css`.
- `prefers-reduced-motion` disables transitions and chart animation.
