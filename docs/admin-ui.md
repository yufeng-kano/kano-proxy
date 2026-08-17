# Web UI

Vue 3 + Vite + TypeScript on Cloudflare Pages (same hostname as API via routes). All copy comes from the message catalog — see [i18n.md](./i18n.md).

## Pages

| Route | Content |
|-------|---------|
| `/` | Redirect to overview or login |
| `/login` | Google sign-in (split brand panel + sign-in panel) |
| `/overview` | Usage and cache-rate dashboard over `request_logs` |
| `/logs` | Per-request log explorer over `request_logs` — newest first, cursor-paged |
| `/providers` | Connected subscription accounts (Claude / Codex / Grok) and custom endpoints, one section per provider |
| `/models` | Searchable catalog of `provider/model` ids, grouped by provider |
| `/groups` | Model groups: bare-name aliases → ordered `provider/model` targets |
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

- **Sidebar** 248px, its own column, never scrolls with content: brand at the top, primary nav below it, then the Changelog link carrying the running version pinned to the bottom of the nav area, and the signed-in user with sign-out in the footer beneath. The footer shows the Google **display name** and the profile photo **in color**. It does not show the email; if a user has no name (rare — Google usually sends one) the name field is blank rather than falling back to the address. The photo is the identity mark — greying it out to keep the chrome quiet made the operator unrecognisable.
- **Active nav item** is marked by a filled pill **and** a step up in font weight — never color alone. The fill on its own is a ~2% luminance delta; the weight is what makes it read.
- **Page header** is sticky at the top of the content region and holds the page title, its primary actions, and (where a page has sections) the section nav. It is the same component on every page.
- **Header actions stay on the title row, right-aligned, at every width** — the range picker, the search field, Refresh, Create key. They never drop to a full-width row of their own beneath the title: the title row is where the page's controls live, and a phone that moves them below the heading spends a whole extra row restating the same layout. Individual controls shrink instead, and the row may wrap internally when they genuinely do not fit, but the actions box itself is never widened to fill the line.
- A control sized by a **fixed** width in that row must be shrinkable, or it wraps its neighbours instead of itself: Models' search is `width` + `max-width: 100%` + `min-width: 0`, so on a phone it gives up width to keep Refresh on the same line. A `width` alone still contributes its full size to the flex line and pushes the next control onto a second row.
- The page header's **frost spans the content region**: it cancels the region's *top* padding so the blur reaches the top edge when stuck, **and** the horizontal gutter so the wash and bottom rule run from the sidebar edge to the region's far edge. Title, subtitle, actions, and section nav stay on the **content column** — the same left and right edges as the cards below — by putting the cancelled gutter back as header padding. The gutter itself is tight (`--space-2` at every width): title, section nav (All / Claude Code / …), actions, and the cards share it, so they move together. A wide inset parks the sticky title far from the region's edges; shrinking only the header (leaving the cards or the tabs on the old gutter) desyncs the section nav from the cards as they scroll under it. Two other failures to avoid: a header whose *words* run wider than the cards reads as two page widths stacked; a header whose *wash* stops at the card column reads as a frosted strip that does not cover the page.
- The header is **chrome, not a surface**: compact (one title row at `--text-md`, subtitle beside it, tightened vertical padding), and its background is the page background (`--topbar-bg` is a translucent tint of `--bg` in both themes, paired with `--topbar-blur`) so a stuck header reads as the page frosting out under the controls — never as a white card slab sitting on the page. The frost is **heavy**: opacity low enough that scrolling cards smear through, blur high enough that the smear reads as milk, not as a ghost of the table. Raising opacity toward opaque `--bg` kills the frost; painting `--surface` turns the header into a card.
- The content region is the scroll container, so `window.scrollY` is meaningless here; scroll restore listens on that element (see [View preferences](#view-preferences-localstorage)).
- That region reserves its scrollbar track permanently (`scrollbar-gutter: stable`). Pages differ in height — Providers' All tab scrolls, Models' bounded card does not — and without a reserved gutter the content column shifts sideways by the scrollbar width on every such navigation, which reads as the layout jumping rather than as a scrollbar appearing.
- `AppShell` publishes its own metrics as inherited custom properties — `--page-top`, `--page-bottom`, `--page-gutter`, and `--page-chrome` (the shell chrome above the region: 0 on desktop, the mobile bar below the shell breakpoint). A page that sizes itself to the viewport, or a header that cancels the top gutter to bleed its blur upward, **reads those** rather than restating the values. A second copy drifts the first time only one of them changes at a breakpoint.
- **The shell fills the whole screen; the scroll region clears the safe area from the inside.** `viewport-fit=cover` extends the page under the home indicator, so a `100dvh` frame that stops short of it leaves a permanent strip of bare `body` below the app — visible on every page, scrollable to nowhere. The frame therefore keeps its full height and the *content region* adds `env(safe-area-inset-bottom)` to its own bottom padding: the background reaches the bottom edge, while content still scrolls clear of the indicator and passes behind it rather than stopping above it. `--page-bottom` stays the design value; the inset is added on top of it, so a page sizing itself to the viewport subtracts the same number it always did.
- `--content-max` caps the content column so a table does not stretch across an ultrawide display. It is sized to *use* a normal laptop/desktop width rather than to a comfortable reading measure — long-form pages cap themselves separately (Changelog at 72ch), so this value only has to keep dense tables usable.

### Anti-scroll rules

The core constraint: **the user should not have to scroll to find things**. Concretely:

- Every page's primary controls live in the sticky header, reachable at any scroll depth.
- Long collections get **in-page navigation instead of stacking**: Providers and Models filter by provider tab (with an All view); Keys splits its two jobs into tabs; the Overview activity card splits its four views into sub-tabs.
- Wide/tall data goes in bounded regions with sticky column headers, not an unbounded page. A table scrolls inside its own card.
- The Overview page targets a single viewport at 1440×900: the three metric cards, then the activity card. Only the By-model table scrolls internally.
- New sections are added to a section nav, never appended to the bottom of an already-long page.

## Responsive

Three breakpoints, and no more: `640` (phone), `768` (tables), `1080` (shell). The content gutter is `--space-2` at every width — it is not a fourth breakpoint.

| Width | Shell |
|-------|-------|
| ≥ 1080px | Full sidebar (248px) + content |
| < 1080px | Sidebar becomes a slide-in drawer behind a header menu button |

No icon rail and no bottom tab bar. With seven destinations the text labels are what make the nav scannable, so a rail would trade legibility for width the content does not need; a bottom bar would cost 56px of the scarcest axis on a phone and read as a mobile app rather than a web app.

- Tables below 768px render as stacked cards (label + value rows), not horizontal scroll — a horizontally scrolling table on a phone hides the columns that matter. `DataTable` does this; a hand-rolled table does not.
- Stat tiles reflow on **container** width, not viewport width: the sidebar's presence changes the content width, and a viewport media query gets that wrong at every shell state. The hero (cache rate) spans two columns until the layout drops to two.
- The chart's plot area is a **fixed height**, never an aspect ratio — an aspect-ratio chart in a wide content region eats the viewport. Bucket-label density thins instead; labels never rotate.
- Dialogs become bottom sheets below 640px, with `env(safe-area-inset-bottom)` clearance.
- **A dialog is never wider than the viewport, whatever it contains.** The panel and its body both carry `min-width: 0`: a flex/grid child defaults to `min-width: auto`, so one wide descendant (the detail modal's chart, which declares a 420px floor) stretches the panel past the screen and pushes the close button off it — the dialog then cannot be dismissed by tapping, only by Escape, which a phone does not have. Content that needs more width than the phone has **scrolls inside** the panel; the panel itself stays put.
- **An open dialog does not scroll the page behind it.** Scrolling a dialog to its end otherwise *chains* to the content region underneath, which scrolls to somewhere the user never chose and stays there once the dialog closes — on a phone that reads as the page having broken. `Modal` therefore locks the region while it is mounted (`overflow: hidden` plus a compensating `padding-right` for the scrollbar's width, so the content does not jump sideways as it locks) and restores it on unmount. `overscroll-behavior: contain` on the panel body is the second layer: it stops the chain at the source rather than relying on the lock alone. Note the region's own `contain` does **not** cover this — that stops scroll leaving the region, not arriving from an overlay above it.
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

`AppShell`, `PageHeader`, `SectionNav`, `Segmented`, `DataTable`, `Card`, `Modal`, `EmptyState`, `Banner`, `StatusDot`, `CopyField`, `UsageBar`, `SkeletonBlock`, `Spinner`, `NavIcon`, `ActionIcon` — plus the Overview-only chart primitives under `components/overview/` (`BarChart`, `ChartTooltip`).

`DataTable` is the one place table markup lives: it owns the sticky header, the tabular-numeral alignment, and the mobile card fallback. A page that hand-rolls a `<table>` will not get those.

An **action column** (Models' Copy, Keys' Edit) declares an explicit `width` *and* an `align`, and sits **last**. Without the width it soaks up the table's leftover space and its header ends up at one edge of a 400px column with the control at the other. It carries no visible header — the control already names itself — but does carry `srHeader`, rendered visually hidden, because a blank `<th>` is an unnamed column to a screen reader reading the table's structure.

A row's control belongs in that trailing column rather than inline beside the field it acts on: inline, it lands at a different x-position on every row, and a column of controls that zigzags is one the eye has to search for on each row.

**Numeric columns are centered**, header and cells together, with tabular numerals. Right-aligning them strands short figures a track's width from the header naming them — SPEND and MIN are much narrower than the columns they head. (For a long time this was moot: `.table th`'s `text-align: left` outweighed a bare `.numeric` selector, so every numeric header was left-aligned over right-aligned figures. Both alignments are now written as `.table th.numeric, .table td.numeric` so the column decides, not the specificity accident.)

Icons are inline SVG on one 16px grid with a 1.4 stroke and round caps/joins: `NavIcon` for destinations, `ActionIcon` for controls. Both are always `aria-hidden` — an icon-only control carries its name in `AppButton`'s `label` (which becomes both `aria-label` and the tooltip), never in the glyph.

`AppButton`'s `label` also applies to a **labelled** button, where it overrides the visible text as the accessible name. That is for controls whose words repeat down a list — three rows each offering "Remove" are one word to a screen reader reading them out of context, so the row's subject goes in the accessible name ("Remove {account}"). Such a name must still **contain** the visible label verbatim (WCAG 2.5.3): a voice-control user says the word they can see.

**Prefer a labelled button to an icon for row actions.** An icon is right for a control that repeats on every page and is learned once (Refresh, Copy, the section gate); it is wrong for a rare, consequential action on a specific record, where a bare glyph makes the user hover to find out which one deletes their data.

**Icon-only buttons are `ghost`** (no border, no fill) unless they are the page's primary action. A bordered square with a glyph in it competes with the card title beside it for the same amount of attention, and a section header carrying two of them reads as a toolbar rather than as a heading with an affordance.

Two implementation notes that are not obvious and cost real debugging time:

- A sticky `<th>` must draw its bottom rule with `box-shadow: inset 0 -1px 0` — a `border-bottom` on a sticky cell scrolls away independently of the cell, leaving the header floating unruled.
- A component that both exports a type and uses `<script setup>` needs a **plain** `<script lang="ts">` block for the export. A second `<script setup>` block is silently dropped along with every macro in it, and the component renders nothing.

## Data freshness

Cache-first everywhere, and **invisible to the user** — no "cached" / "refreshing" / "last updated" text on any surface. Freshness is the app's job.

- Paint the `localStorage` cache immediately (even if stale); fetch only if the cache is older than **2 min**, or on an explicit user Refresh.
- Server-data caches live in **`localStorage`**, not sessionStorage, so a reopened tab or browser restart paints the last known data instantly and multiple tabs share one cache. Each entry is a versioned envelope `{ v, savedAt, data }`; a bumped `CACHE_SCHEMA_VERSION` or malformed blob reads as a miss, never as trusted data.
- Providers page polls every **2 min** *while visible* (see "Polling stops when the page is hidden" below); Refresh sets `?refresh=true`, which bypasses both the models KV cache and the 2 min server-side usage cache.
- **Per-account usage is cached 2 min server-side in D1** (matching this page's poll interval), shared across every device and tab, because the localStorage TTL is per-device and N devices meant N× upstream calls (see [providers.md](./providers.md) § Usage cache). The frontend TTL is still the first line of defense — it keeps same-device tabs and rapid re-visits off the network entirely — but it is no longer the *only* one. The **models catalog keeps its KV cache at a 1h TTL** because the client-facing `GET /openai/v1/models` / `GET /anthropic/v1/models` are hit by API clients with no frontend cache.
- On refresh failure, keep showing cache and surface a non-blocking error.
- A background refresh shows no spinner. A *user-initiated* refresh shows progress on the control they pressed — that one is their action, so it gets feedback.
- First load with no cache shows skeletons shaped like the content, never a bare "Loading…".
- Never store access tokens, refresh tokens, session secrets, or a custom provider's API key in local UI cache — a cached custom-provider row carries only non-secret fields the `GET /api/custom-providers` response already returns (`key_mask`, never the key). What *is* cached (account emails/labels, usage percentages, model ids, key masks) is non-secret display data, cleared by the logout sweep.
- Cache keys are scoped to the signed-in user id: `kano-proxy:custom-providers:{userId}`, `kano-proxy:usage:{userId}:{days}`, etc.
- **Changelog is the one exception**: TTL is **1 hour** and the key `kano-proxy:changelog` carries **no user id** — the data is identical for every operator and holds nothing user-identifying. The logout sweep still clears it. See [changelog.md](./changelog.md).

### Polling stops when the page is hidden

A background poll against upstream billing APIs must not run for a page nobody is looking at. The Providers page gates its interval on the [Page Visibility API](https://developer.mozilla.org/en-US/docs/Web/API/Page_Visibility_API) (Baseline since 2015; identical semantics in Chrome, Firefox and Safari): `hidden` covers a background tab, a minimized window, and a screen that is off or locked.

- **Hidden → `clearInterval`.** Stop entirely; do not rely on the browser's own timer throttling. Chrome's intensive throttling (1/min) needs the page hidden **>5 min** plus ≥5 chained callbacks, silence >30s and no WebRTC — before that it still checks ~1/sec; Firefox Desktop's background floor is 1s. Both are at or below a 2 min poll, so throttling barely slows us down.
- **Visible → fetch once, then restart the interval.** The one-shot catch-up is what keeps a returning user from reading a stale page; it still respects the localStorage TTL, and the 2 min server cache absorbs the rest.
- **Check `visibilityState` at mount, not just on the event.** `visibilitychange` fires only on a *transition*, so a tab born hidden never gets one. This is load-bearing for **Firefox pinned tabs**: `browser.sessionstore.restore_pinned_tabs_on_demand` defaults to `false` (ordinary tabs restore lazily, pinned ones do not), so a pinned Providers tab boots and runs JS at browser startup without ever being looked at. Event-only wiring would poll upstream forever there.
- **Also re-check on `pageshow`.** A bfcache restore (user leaves the site, presses Back) does not re-run `onMounted`; the heap thaws with polling already stopped, and Safari's `visibilitychange`-on-restore behavior is historically unreliable. Without this the page can wedge, visible and never refreshing.
- **Never substitute `focus`/`blur`.** MDN is explicit that they do not tell you the page is hidden — two side-by-side windows would falsely stop polling.
- **Known and accepted:** a window fully *obscured* by another app still reports `visible` and keeps polling. Page Visibility measures browser-considered visibility, not pixels, in every browser; the tab is one Cmd+Tab from being read, so its data should be warm.
- The OAuth dialog's device-flow poll (`AddAccountDialog.vue`) is deliberately **exempt** — the user is approving in another tab, so that page is *supposed* to be hidden while it polls.

### View preferences (`localStorage`)

Server **data** caches (above) and **view preferences** — what the user picked, not what the server said — both persist in `localStorage`, but stay separate modules: data entries are user-id-scoped versioned envelopes swept on logout, while preferences live under a single `kano-proxy:prefs` key that survives logout so a reopened tab lands where the user left off.

- Stored: last visited route path, per-route scroll offset **of the content region**, the Overview range (24h/7d/30d) and activity view (tokens / requests / cache / models), and the Providers, Models, and Logs pages' active provider filter.
- **Never** stores tokens, session state, emails, or any server payload — only enum-ish UI choices and integers. Unlike the server-data caches it is therefore **not** user-id scoped and **not** swept on logout: it holds nothing user-identifying, and a shared machine reveals only a route name.
- Every read is validated against the current allowed values and falls back to the default on anything unexpected (stale schema, hand-edited storage, removed route). A malformed blob is discarded, never trusted.
- Restore-on-boot only replays a route the router still knows and the signed-in user may see; the auth guard runs unchanged, so a persisted path never bypasses login.
- Scroll restore waits for the page's first data paint, then sets the offset on the content region once; a user scroll during restore cancels it rather than fighting the user.

## Overview page

Route `/overview`; signed-in `/` redirects here. Data source: `GET /api/usage/summary?days=1|7|30` (session auth, see [auth.md](./auth.md)) aggregating `request_logs` — **live rows only, no fabricated numbers**; an empty range renders an explicit empty state with a link to Keys. The summary is **scoped to providers that still exist** (builtins + the user's live custom slugs); rows whose provider was deleted, and junk prefixes from invalid-model 400s, are excluded server-side. Model names render uniformly as the canonical `provider/model` id on every Overview surface — the metric cards' top-model lists, the detail modal, chart legends/tooltips, and the By-model table — with no friendly-name remap.

- Range picker (24h / 7d / 30d, `days=1|7|30`, default 7) lives in the sticky page header. Series buckets are hourly for 24h, daily otherwise (UTC bucket keys from the API, formatted client-side; missing buckets zero-filled client-side). Time runs **oldest-left → newest-right**.
- **Metric cards** (row 1): three equal cards — **Spend** (estimated USD, [pricing.md](./pricing.md)), **Requests**, **Tokens** (input + output). Each carries the range total as its headline, a mini stacked-column chart of that metric over the range (axis-free), and the top models by that metric (top 4 + "Others") with their values. An expand control on each card opens a **detail modal**: the full-size stacked chart plus a per-model Min / Max / Avg / Sum table over the range's buckets (Avg over non-zero buckets), sorted by Sum desc. Spend annotates how much of the range is estimated (read-time priced) or unpriced rather than silently mixing.
- **Activity card** (row 2): one card, sub-tabs — **Tokens** / **Requests** / **Cache** / **By model**:
  - **Tokens** (default), **Requests**: full-width stacked-column chart of that metric per bucket, one segment per model.
  - **Cache**: stacked cached vs uncached input tokens per bucket (two fixed series: `--chart-input` cached, `--chart-input-soft` uncached), so cache savings read at a glance.
  - **By model**: the detailed table — requests, errors, input/output tokens, cache read/write, cache rate, spend; one row per **(provider, model)**, sorted by total tokens desc, scrolling inside the card with a sticky header. When cache data covers only part of a model's requests, the coverage is annotated instead of silently mixed. Requests addressed through a model-group alias count toward the **expanded** target's row, and traffic is summed across accounts — the dashboard aggregates what actually ran; the per-request account/alias detail lives on the [Logs page](#logs-page).
  - The card header carries the range's error count and average latency as compact secondary stats, so those metrics survived the tile removal.
- Charts are hand-rolled inline SVG — **no charting dependency**; colors/typography from `styles.css` tokens only, legible in both themes. One shared `BarChart` primitive draws every stacked-column view (mini and full): dotted horizontal grid, nice ticks, thinned x labels (never rotated), fixed plot height, measured viewBox. Model identity uses `--series-1..6` + `--series-other`, assigned by the model's rank in the range totals so a model keeps its color across buckets and across the three metrics. Every chart ships a legend (full size), a hover/focus tooltip, and a table twin (the detail modal / By-model tab) so identity and values are never color-only.
- Requests without usage data (`NULL` token fields — see [database.md](./database.md)) count toward request/error totals but are skipped by token/cache aggregates; the UI surfaces that coverage rather than hiding it.

### Series shape

`series[]` carries a per-model dimension so every stacked view needs no second request. Each point is one `(bucket, provider, model)` group: `bucket`, `provider`, `model`, `requests`, `prompt_tokens`, `completion_tokens`, `cache_read_input_tokens`, `cache_known_requests`, `cost` (estimated USD for that group, `null` when wholly unpriced). Sparse — only groups with at least one row; the client zero-fills the bucket grid and folds the model tail into "Other". Bucket-level totals are the client-side sum over a bucket's model points, not a separate field.

The `models[]` breakdown is the same **(provider, model)** grain as the series, summed over the whole range: rows served by different accounts and rows addressed via a group alias fold into one entry — the summary carries no account or alias dimension at all (that per-request detail is the Logs page's job, `GET /api/logs`). A per-account/per-alias split multiplied table rows without a question the dashboard answers. Any change to this response shape bumps `CACHE_SCHEMA_VERSION` so stale cached envelopes read as a miss.

## Logs page

Route `/logs`, nav item **Logs** directly below Overview. Data source: `GET /api/logs` (session auth, [auth.md](./auth.md)) — a newest-first, cursor-paged listing of the caller's own `request_logs` rows. This is the per-request companion to Overview's aggregates: the account and group-alias detail the By-model table folds away lives here, as does the error evidence (`error_code`, `upstream_status`) that previously required `wrangler d1 execute --remote`.

- **No live-provider scoping** — unlike the usage summary, rows from since-deleted custom endpoints and invalid-model junk prefixes still render. It is a log; hiding rows hurts diagnosis.
- **One bounded card** filling the content region (same as Keys/Models), rows scrolling inside with a sticky header; a **Load more** control at the list's end appends the next page.
- **Columns**: Time, Model (canonical `provider/model`, with the localized "via {alias}" tag when `group_name` is set), Account (resolved label; removed-account tag when the id no longer resolves; `—` when `NULL`), Type, Status, Input, Cache read, Cache write, Output, Cost, Latency. Numeric columns follow the shared centering rule; secondary columns hide on mobile (DataTable's card fallback carries the rest).
- **Type** is derived server-side, never stored: `oauth` when `provider` is a builtin subscription pool, `api` otherwise (custom endpoints, including deleted ones) — the client keeps no builtin list of its own.
- **Status** is never color-only: success rows show the HTTP status quietly; failure rows show the `error_code` as a badge. `upstream_status` renders in the row detail, since `status_code` alone hides it for eager streams ([logging.md](./logging.md)).
- **Row detail**: clicking a row opens a modal with the row's full fields — timestamps, ids, API key name, `status_code` / `upstream_status` / `error_code`, the token and cost breakdown. There is no message content to show and none is fetched ([logging.md](./logging.md)).
- **Filters** in the sticky header, both applied server-side: a provider filter (All + builtins + the user's live custom slugs; persists as a view preference like Models') and an errors-only toggle (`error_code` set or `status_code >= 400`).
- **Pagination**: keyset cursor over `(created_at, id)` descending, page size 50 (max 100), covered by `request_logs_user_created_idx`. The response returns `{rows, next_cursor}`; a `null` cursor means the end.
- Cache-first like every page: the **first page of the unfiltered view** caches under `kano-proxy:logs:{userId}` (standard 2 min TTL, versioned envelope, logout sweep); filtered views and Load more results are fetched, not cached.
- `cost` NULLs are filled at read time with the shared price-table resolver, same as the summary ([pricing.md](./pricing.md)); a row that stays unpriced renders `—`, never `$0.00`.

## Providers page

Tabs in the sticky header, same pattern as Models: **All**, one tab per builtin provider, and **Custom** — each with its connected-account count. All shows every section stacked; a provider tab shows only that provider's card. One panel at a time (real tab semantics), no anchor-scrolling.

Each section's **Add** control is an icon-only ghost button (`plus`) in the card header — Add account / Add endpoint as its tooltip and accessible name. The page is read far more often than it is added to, so the create affordance sits at icon weight rather than as a bordered button on every section.

**Add hides while the gate is open.** Editing the pool and adding to it are different jobs, and the open gate says which one the section is currently in; leaving a create control beside the Done toggle offers an action that has nothing to do with the mode the user just entered.

Row actions are **edit-gated at the section level**: one pencil toggle in the card header, beside that section's Add control, and it reveals the actions on **every row in that section at once**. The page is for looking at pool health — a pencil repeated down each row is the same affordance restated once per account, and it is not what the rows are read for.

While the gate is off, a row is identity and usage only, so destructive controls are never one accidental click away. While it is on, each row's actions occupy the blank space at its right edge, on the identity line — nothing below reflows, and the section's controls stay in one column down the right edge (Add and the gate in the header; each row's actions beneath them).

The gate carries `aria-pressed` and swaps to a check while open — the same control opens and closes it, so its state has to be audible as one. It is deliberately independent of loading state: the 2 min background poll must not close the actions a user is aiming at. It renders only when the section has rows, since there is nothing to reveal otherwise.

Revealed actions are **labelled buttons, not icons**. A row action is a rare, consequential operation on a specific account, and a bare glyph makes the user hover to find out which one removes it — a tooltip is not a label. The gate already keeps them out of the resting row, which is what buys the width to spell them out. Destructive actions stay visually distinct (`danger` tone) on top of their label, never distinguished by position alone.

Subscription accounts (Claude / Codex / Grok):

- Status dot: active / standby / benched / unusable, always paired with a text label — never color alone.
- Progress bars per usage window (5h, Week, …). `utilization` is always a **percent (0–100)**, never a 0–1 fraction — adapters normalize upstream values to that scale, so the bar renders it directly (clamped and rounded) with no rescaling heuristic.
- **Do not show a usage-probe failure the operator cannot act on.** Two errors stay off the row even when the API returns them: Codex's edge / bot-wall block (chat still routes), and **`usage 429`** (Anthropic's usage endpoint rate-limited the probe; Messages traffic is a different budget and the last good windows stay on the bars). A yellow banner that restates "usage 429" under otherwise-healthy bars is noise. Token / auth failures still surface — those are actionable.
- Actions (behind the section's gate), labelled: **Resume** (nulls the D1 bench columns so the account is eligible again; hidden unless `status === 'benched'` — not shown for `unusable` / "Needs attention", which is a token problem, and not for a usage-window limit, which is not a pause), **Primary** (raises this account's priority so requests route through it first; hidden when it already is), **Rename**, **Remove** (`danger`, confirms first). Resume is not destructive and does not confirm; it writes `POST /api/providers/:provider/accounts/:id/unpause` ([auth.md](./auth.md)) and then reloads that provider the same way Primary does. The next bench-status upstream response will pause the account again — unpause is not a lock. Add account → provider-specific sign-in flow in a dialog.
- **Strategy** (behind the section's gate, in the card header beside Add/gate): a labelled select for the pool's routing strategy — reads from the `strategy` field on `GET /api/providers/:provider/accounts`, writes `PATCH /api/providers/:provider` ([auth.md](./auth.md)). **Ordered** is the only option today (the select still renders — it is the seam future strategies appear in, and it tells the operator the pool *has* a routing policy); the option's descriptive line states the semantics ("priority order, first usable account"). Section-level config, so it lives with the section's other config affordances behind the edit gate, never on rows. Builtin sections only for now — custom endpoints default to `ordered` server-side and grow the same control when multi-key UI lands.
- The promote button reads **Primary**, the same word as the status badge on the row that already is one. That repetition is deliberate: the badge and the button are the two halves of one fact, so the word the user presses is the word they get back. They never appear on the same row — the button is hidden on the primary account — so the two readings cannot collide. Accessible names disambiguate anyway (`Make {name} primary`).
- **Rename** opens a small dialog writing `custom_label` via `PATCH /api/providers/:provider/accounts/:id`; blank clears it and the row falls back to the upstream email/username. A rename is display-only — it never touches which account is primary, and the upstream identity sync never overwrites it (see [database.md](./database.md)). The row shows the custom name as its title with the upstream identity beneath it, so a renamed account is still traceable to the real account it proxies.

Custom endpoints (`GET /api/custom-providers` — see [auth.md](./auth.md)):

- Name, a format badge (`OpenAI` | `Anthropic`), the `slug/*` model-prefix hint, the base URL, the key mask (e.g. `sk-abc…f3a2`), and a status dot (**active** / **benched** only — a static key has no usage window to show). An OpenAI-format row with a `count_tokens_url` set adds one secondary line for it, in the same style as the base URL; rows without one add nothing (the common case must not grow a permanent empty slot).
- Actions (behind the section's gate), labelled like the account rows: **Resume** (hidden unless `status === 'benched'`; `POST /api/custom-providers/:id/unpause` — same semantics as the account Resume), **Test** (`POST /api/custom-providers/test` with `{id}`, inline result), **Edit** (opens the endpoint dialog), **Remove** (`danger`, confirms first since it also deletes the stored key).
- **Reordering** (behind the same section gate, when there are 2+ endpoints): each row gets **Move up** / **Move down** icon buttons (disabled at the ends) and a drag handle; both write the whole new order via `PUT /api/custom-providers/order` (see [auth.md](./auth.md)). The order is display only — it does not change routing or which key is used. The list reorders optimistically and rolls back to the server order on failure, with an inline warning. Dragging is pointer-based and must never be the only way to reorder: the buttons carry the same capability for keyboard and touch users, are reachable in tab order, have accessible names (`Move {name} up`), and announce the new position via a live region. The drag handle is `aria-hidden` decoration on top of that.
- **Add endpoint** dialog: format toggle (immutable once saved); name with a slug auto-generated from it (editable before first save, then locked — immutable server-side too); base URL with a **live preview of the resolved endpoint** as the user types (`{base}/chat/completions` for OpenAI, `{base}/v1/messages` for Anthropic, matching the literal-concatenation rule in [providers.md](./providers.md)); API key as `type="password"`, **never pre-filled or echoed** — on edit, blank means "keep the existing key" (matches the backend's blank-means-keep contract, see [auth.md](./auth.md)); models mode toggle (auto / manual, with a textarea for manual ids); a **Token-count URL** field shown **only for the OpenAI format** — optional, a complete URL (not a base), explained by a hint that says what it buys ("Claude Code asks for a token count; an OpenAI endpoint has none. Point this at an Anthropic-compatible `/v1/messages/count_tokens` if your gateway has one — leave empty and that request keeps failing as it does today"), pre-filled on edit (it is not a secret) and cleared by emptying it; a **Test connection** button that calls the same endpoint with the in-progress form values and renders the result inline without blocking Save.

## Models page

- Sources (only providers with a bound usable account are queried):
  - Claude Code: live upstream `GET /v1/models` with OAuth
  - Grok: live upstream `GET /v1/models` with OAuth
  - Codex: no public / third-party models list → empty state pointing at the user's plan (see [providers.md](./providers.md))
  - Custom providers: one group per user-defined endpoint, from the same `GET /api/models` payload; manual list or live-with-fallback depending on that provider's `models_mode`
- Provider groups are **dynamic**, rendered from whatever `providers` the response lists, so a new custom endpoint appears without a UI code change.
- A search box filters across every group by model id and display name; a provider filter in the header narrows to one group. Both are client-side over already-loaded data — no request per keystroke. The active filter persists as a view preference.
- Session API: `GET /api/models` (`?refresh=true` bypasses the 1h server KV cache). Client-facing `GET /openai/v1/models` and `GET /anthropic/v1/models` return the same live catalog; ids are always `provider/upstream`.
- Copy model id as `provider/upstream` — works on **both** OpenAI and Anthropic bases. The row's copy control is an icon button in a fixed-width right-aligned column, and confirms in place by swapping to a check; the whole row is also click-to-copy, so the pointer target is the row rather than a 28px square.

## Groups page

Route `/groups`, nav item **Groups** between Models and Keys. Data: `GET /api/model-groups` ([auth.md](./auth.md)), cache-first under `kano-proxy:model-groups:{userId}` with the standard 2 min TTL and logout sweep. Contract: [providers.md](./providers.md) § Model groups.

- **One bounded card** filling the content region (same as Keys/Models), list scrolling inside with a sticky header. Columns: **Name** (display label), **Aliases**, **Targets**, **Updated**, then the unlabelled trailing edit column — the Keys-page pattern, not the Providers edit-gate (groups are few and editing is the page's whole job).
- **Aliases are the model ids.** Each alias renders as a click-to-copy chip with the same in-place check confirmation as the Models page — an alias is exactly what the client puts in `model`, so copying one is the row's primary read action. The display name is a label only and is not copyable-as-id.
- **Targets** render as the ordered list in priority order (first = tried first): each entry shows its `provider/model` **and its account** — the resolved `account_label` when pinned, a localized "Any account" tag when not. A pinned account that no longer exists renders a warning tone, not silence — the target will be skipped at request time and the row should say so. On narrow widths the DataTable card fallback keeps the order readable as a numbered stack. The target row is deliberately structured with room to grow: future balancing adds per-target facts (weight, live usage %) as additional cells/chips on the same row, not a redesign.
- **Current-route indicator:** the target the router would dispatch right now (`routing.current_target_index` from `GET /api/model-groups` — [auth.md](./auth.md)) carries a localized **Current** badge; an unusable target renders in a warning tone with its localized reason and recovery time ("limit — until {time}", "benched — until {time}", "unavailable" when `unusable_until` is unknown). Status is never color-only: badge and reason are text, tone is reinforcement. The indicator is computed from the **same stored facts dispatch uses** (D1 bench columns + usage snapshots, no upstream calls), so it shows what the next request would actually do — and its staleness is exactly the router's own staleness, which is the honest thing to display. Freshness is additionally bounded by the page's cache-first TTL; a user Refresh re-reads current facts.
- **Create group** button in the sticky page header opens the dialog; **Edit** (pencil, ghost) sits in the trailing column; **Delete lives inside the edit dialog** as the destructive footer action, confirm-first — same reasoning as key revoke: rare, irreversible, one deliberate step.
- **Create/edit dialog — three columns: identity | pick | order — and they must read as one surface.** The dialog is wide: **3/4 of the viewport width** (capped by the viewport margins on small desktops; requested 2026-08-13) — and **tall**: the picker and target-list regions size themselves to the viewport height (floor at the old fixed heights, ceiling against unscannable lists) instead of leaving half a tall display as empty overlay (rejected 2026-08-13; `Modal`'s `wide` size lifts the fixed 760px panel cap for the same reason). Its three columns — ① group identity, ② model picker, ③ ordered targets — share a single visual frame: one continuous background, one hairline divider between each adjacent pair of columns (both running the full panel height), all three column headers on one aligned row, identical vertical padding — never separately-boxed panels sitting side by side (the v3.2.0 two-card layout was rejected 2026-08-13; the v3.3.0 layout that stacked identity above the picker in one left column was revised to three columns the same day).
  - **Column ① — identity:** the **display name** field (free text, 1–64 chars, a label not an id; its hint renders **between the label and the input** — guidance to read before typing, matching the aliases block below it), beneath it the **aliases editor**: existing aliases as removable chips plus an inline add field (Enter/comma commits), microcopy stating each alias is a model id callable on both bases and that clients may use any of them, and beneath that the **Strategy** select (`strategy` on the group — [auth.md](./auth.md)); **Ordered** is the only option today, its descriptive line stating "targets tried in list order" — same rendered-even-with-one-option reasoning as the Providers page control. Client-side validation mirrors the server rules (alias: 1–128 chars, no whitespace, no `/`; 1–10 per group) with inline errors; the cross-group-uniqueness `400` from the server renders inline naming the conflicting alias.
  - **Column ② — picker, inverted-L navigation:** a horizontally scrollable **provider tab strip** across the top (same pattern as the Models page tabs: builtins + one tab per custom endpoint, overflow scrolls sideways), and a vertical **account rail** down the left edge; together they frame the model list, which fills the remaining region. Rail entries are **full-bleed list rows** — the selection fill runs the rail's entire width; a bordered card inset in the rail reads as a widget, not a selection (rejected 2026-08-13). The ≤640px chip strip keeps per-item outlines, since a horizontal strip has no rail edge for a fill to run to. Selecting a tab loads that provider's accounts into the rail (same accounts data the Providers page loads, fetched lazily per provider, cached — display name `custom_label` \|\| upstream label); selecting an account reveals the provider's models (searchable, from the already-loaded `GET /api/models` catalog — client-side filter, no request per keystroke; the list packs from the top whatever its count — a stretched grid that spreads two models across a tall track was rejected 2026-08-14). Clicking a model **adds it to the right pane pinned to the selected account**. **Catalog only — there is no free-text add row** (removed 2026-08-14: entering arbitrary ids is not this dialog's job; the wire contract still accepts any valid `provider/model`, so the REST API remains the way in for an uncataloged id). The picker runs **flush between the column dividers** — it cancels its column's horizontal padding, because padded gutters either side read as empty bands splitting the surface (rejected 2026-08-14); the column head keeps the padding that aligns the three heads.
  - **Every new target pins an account — there is no "Any account" entry in the rail.** (Deliberate UX decision 2026-08-13: group order is the routing authority, so the picker always says which account a target means.) The backend keeps accepting unpinned targets — `account_id` stays optional on the wire — and an existing unpinned target still renders on the right pane with the localized "Any account" tag; the UI just no longer *creates* new ones. A provider tab with zero bound accounts shows an empty-rail state pointing at the Providers page instead of a model list.
  - **Custom endpoints in the rail:** a custom provider's API key is itself an `upstream_accounts` row; `GET /api/custom-providers` exposes its `account_id` (see [auth.md](./auth.md)) so the rail lists that key as the single account entry (labeled by the endpoint's name/key mask) and pinning works uniformly. Multi-key custom providers, when they get UI, extend this rail for free.
  - **Column ③ — the ordered target list** (= priority, first is tried first): each row shows model + pinned account (or the legacy "Any account" tag), **Move up / Move down** (disabled at the ends, accessible names `Move {target} up`, position announced via live region) and **Remove**. Buttons are the baseline; a drag handle, if added, is decoration on top — same rule as custom-endpoint reordering. Microcopy above the list states the semantics in one line — requests try targets in this order and use the first one with a usable account — and renders **only while the list is empty**: once targets exist the ordered list speaks for itself and the paragraph is just distance between the head and the data (requested 2026-08-13). "Same model, different account" is naturally two clicks in this design (pick account A → model, pick account B → same model); duplicate model+account pairs are rejected inline at add time, matching the server.
  - **Narrow widths:** when the viewport cannot afford three columns (below the shell's table breakpoint), the columns stack in ①→②→③ order with the same hairlines as horizontal dividers; below 640px (dialog is a bottom sheet) additionally the account rail collapses into a horizontally scrollable chip strip under the tabs, so the inverted L flattens into two rows.
  - Save is disabled until ≥1 target.
- **Empty state** explains the two jobs (map a hard-coded client model name; join the same model across accounts) with a Create group action.
- **Models page:** the catalog's fixed `group` section renders with a localized **Groups** section label; its rows copy the bare name (the row's id **is** the copyable model id — no `provider/` prefix). No UI code change should special-case beyond the label: sections stay dynamic.

## Keys page

Two tabs in the sticky header: **Keys** and **Connect** (the client setup details).

Key creation and editing run in a **dialog**, opened from a Create key button in the page header (Keys tab only):

- **Create**: name, optional spend limit (USD; empty = unlimited), reset interval (`daily`/`weekly`/`monthly`/`total`), and an include-OAuth toggle (whether subscription-provider traffic counts — see [pricing.md](./pricing.md)). On success the **same dialog** switches to a done step: the plaintext key in an emphasized copy field with the shown-once warning, dismissed with Done. The list refreshes behind it — no separate banner block above the table; the one place the plaintext ever exists is inside that dialog step.
- **Edit** (pencil icon, ghost) in a trailing action column at the **far right**: same form minus the key material — rename and adjust the limit fields via `PATCH /api/keys/:id`. Editing never re-shows a key. It sits at the row's end rather than beside the name, because inline it lands at a different x-position on every row (names differ in length), and a column of controls that zigzags is one the eye has to search for on each row.
- **Revoke lives inside the edit dialog**, as a destructive action in its footer — not as a column. Revoking is rare and irreversible, so it costs one deliberate step (open the key, then confirm) instead of sitting one stray click from every row. It still confirms before firing, and closes the dialog on success.
- Columns are **Name**, **Key**, **Spend**, **Last used**, then the unlabelled edit column — down from seven. Created is dropped: a key's age answers nothing the last-used column doesn't answer better, and every column removed is width the spend figures get back.
- **Spend** reads `"$3.20 / $50.00"` — spent over the window's ceiling — from `GET /api/keys`' `window_spend`. An unlimited key shows the spend alone (`"$3.20"`) with no denominator and no "No limit" annotation: the absent ceiling *is* the statement, and the extra label was a second thing to read that said nothing new.
- Spend is **centered under its header** like every numeric column (§ Component primitives), not stranded at the far edge of its track.

**Both tabs' cards fill the content region**, so switching between them does not resize the page. Keys needs the bounded card anyway (sticky header, list scrolling inside it — § Anti-scroll rules); Connect is short and would otherwise hug its content, which made the card jump between two heights on every tab switch. A tab switch changes what is on the surface, never how big the surface is. This matches Models, whose catalog card fills the region the same way.

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
