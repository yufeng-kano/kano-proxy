# Admin UI

Vue 3 + Vite + TypeScript on Cloudflare Pages (same hostname as API via routes).

## Pages

| Route | Content |
|-------|---------|
| `/` | Redirect to dashboard or login |
| `/login` | Google sign-in (split brand panel + sign-in panel) |
| `/accounts` | Two groups: subscription pool cards (Claude / Codex / Grok — usage bars, add/promote/remove) and custom endpoint cards (user-defined BYO endpoints — status only, add/test/edit/remove) |
| `/models` | Catalog of `provider/model` ids; available when user has a bound account. Grouped by provider, including the user's custom endpoints |
| `/keys` | List / create / revoke API keys; show base URLs copy blocks |
| `/usage` | Optional request log summary (no content) |

## Theming

- All colors live as CSS custom properties on `:root` in `apps/web/src/styles.css`. Components and pages reference tokens only — no hardcoded hex/rgb outside that token block.
- Light and dark are both first-class: dark values are declared in a single `@media (prefers-color-scheme: dark)` override of the same token names. `:root` sets `color-scheme: light dark` so native controls, scrollbars, and form widgets follow the OS.
- No in-app theme toggle — the UI follows the OS preference. Adding a manual toggle requires a docs update first.
- `--accent` / `--accent-fg` invert between themes (near-black button on light, near-white on dark). Anything using `--accent` must stay legible after that flip.
- The login brand panel is intentionally dark in **both** themes; it uses its own local values, not `--surface`.

## Login page

- Two-column split: dark brand panel (left) + sign-in panel (right). Collapses to a single column below **860px**, where the brand panel becomes a compact header strip.
- Brand panel content: enlarged `k` mark, product pitch, and the provider list sourced from the shared `PROVIDERS` constant in `apps/web/src/types/index.ts` (single source of truth with Accounts/Models — do not retype provider names).
- Sign-in button follows Google Sign-In branding: white surface, `#dadce0` border, official four-color G mark inlined as SVG (no CDN/remote asset). Dark theme uses Google's dark variant (`#131314` surface, `#8e918f` border).
- Auth errors render in-panel via the shared `.banner.error` style.
- Site footer pinned to the bottom of the sign-in panel: `© <year> <site name>` on the left, contact `mailto:` link on the right. The year is computed at render (`new Date().getFullYear()`) so it never goes stale.
- Footer values come from `apps/web/src/config/site.ts`. The contact address is deploy-specific and read from **`VITE_CONTACT_EMAIL`** (`.env.development` locally, `.env.production` or a Pages build-environment variable for deploys). There is **no fallback address** — when the variable is unset `contactEmail` is empty and the footer link is not rendered, so a stale default can never ship. Do not hardcode a contact address in a component.
- `SITE.name` in the same file is the display brand for every user-facing surface: login wordmark, footer copyright, and the signed-in topbar. `index.html` repeats it in `<title>` because the static shell renders before the app boots — keep those two in sync when renaming. The `sk-kano-proxy-` API key prefix and the `kano-proxy:*` sessionStorage keys are wire/storage identifiers and are **not** renamed with the brand.
- Login-specific CSS lives in `LoginPage.vue` `<style scoped>`, not in `styles.css`. The one non-scoped rule there (`html:has(.login-page)`) exists so the reserved scrollbar gutter matches the panel instead of showing a stripe of `--bg`.

## UX rules

- **Cache-first** for account lists, usage, models, and custom providers: paint `sessionStorage` immediately; network only if cache older than **90s** (or user clicks Refresh).
- Accounts page polls every **90s** without forcing backend cache bust; Refresh button sets `?refresh=true` (bypass server KV).
- Backend also caches per-account usage in KV for **90s** to avoid provider 429 (see `apps/api/src/pool/usage_cache.ts`); custom-provider `models_mode=auto` catalog lookups use the same 90s KV cache (see [providers.md](./providers.md)).
- On refresh failure, keep showing cache and surface a non-blocking error.
- Never store access tokens, refresh tokens, session secrets, or a custom provider's API key in local UI cache — a custom provider's cached row carries only the non-secret fields the `GET /api/custom-providers` response already returns (`key_mask`, never the key).
- Cache keys scoped to the signed-in user id. Custom providers: sessionStorage key `kano-proxy:custom-providers:{userId}`, same 90s cache-first / background-refresh convention as accounts and models.

## Account row (align lincy Proxy page)

Subscription pool cards (Claude / Codex / Grok):

- Status dot: active / standby / benched / unusable
- Progress bars per usage window (5h, Week, …)
- Promote / remove
- Add account → provider-specific login UI

Custom endpoint cards (`GET /api/custom-providers` — see [auth.md](./auth.md)), listed in their own group below the subscription cards:

- Name, a format badge (`OpenAI` | `Anthropic`), a `slug/*` model-id hint so the user knows what to type as `model`, the base URL, the key mask (e.g. `sk-abc…f3a2`), and a status dot (**active** / **benched** only — no standby/unusable nuance, no usage bars: a static key has no usage window to show).
- Row actions: **Test** (calls `POST /api/custom-providers/test` with `{id}`, shows the inline result — see below), **Edit**, **Remove** (calls `DELETE`, confirms first since it also deletes the stored key).
- **Add endpoint** dialog: format toggle (`OpenAI` / `Anthropic`, immutable once saved); name field with a slug auto-generated from it (editable before first save, then locked — slug is immutable server-side too); base URL field with a **live preview of the resolved endpoint** as the user types (e.g. typing a base URL shows `{base}/chat/completions` for OpenAI or `{base}/v1/messages` for Anthropic, matching the literal-concatenation rule in [providers.md](./providers.md)); API key as a `type="password"` field that is **never pre-filled or echoed** — on edit, a blank field means "keep the existing key" (matches the backend's blank-means-keep contract, see [auth.md](./auth.md)); models mode toggle (auto / manual, with a textarea for manual model ids when manual); a **Test connection** button that calls `POST /api/custom-providers/test` with the in-progress form values (pre-save shape) and renders the result inline (`ok:true` + sample models, `ok:true` + "no models endpoint" note, or `ok:false` + error) without blocking Save.

## Models page

- Model sources (only providers with a bound usable account are queried):
  - Claude Code: live upstream `GET /v1/models` with OAuth
  - Grok: live upstream `GET /v1/models` with OAuth
  - Codex: no public / third-party models list → empty + links to official docs (see [providers.md](./providers.md))
  - Custom providers: one group per user-defined endpoint (dynamic — as many groups as the user has created), sourced from the same `GET /api/models` payload; manual list or live-with-fallback depending on that provider's `models_mode` (see [providers.md](./providers.md))
- Provider groups on this page are **dynamic**, not a fixed three — they render from whatever `providers` the `GET /api/models` response lists, so a newly-added custom endpoint appears without a UI code change.
- Session API: `GET /api/models` (`?refresh=true` bypasses 90s KV cache)
- Client: `GET /openai/v1/models` and `GET /anthropic/v1/models` return the same live catalog; ids are always `provider/upstream`
- Copy model id as `provider/upstream` — works on **both** OpenAI and Anthropic bases

## Keys page

Show:

```text
OpenAI base:  https://<your-domain>/openai/v1
Anthropic:    https://<your-domain>/anthropic
```

Bases are derived from the current deploy host (`VITE_API_ORIGIN` locally, same-origin in production) — not hard-coded.
