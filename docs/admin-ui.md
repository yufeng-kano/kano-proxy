# Admin UI

Vue 3 + Vite + TypeScript on Cloudflare Pages (same hostname as API via routes).

## Pages

| Route | Content |
|-------|---------|
| `/` | Redirect to dashboard or login |
| `/login` | Google sign-in (split brand panel + sign-in panel) |
| `/accounts` | Provider account cards (Claude / Codex / Grok): usage bars, add/promote/remove |
| `/models` | Catalog of `provider/model` ids; available when user has a bound account |
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

- **Cache-first** for account lists, usage, and models: paint `sessionStorage` immediately; network only if cache older than **90s** (or user clicks Refresh).
- Accounts page polls every **90s** without forcing backend cache bust; Refresh button sets `?refresh=true` (bypass server KV).
- Backend also caches per-account usage in KV for **90s** to avoid provider 429 (see `apps/api/src/pool/usage_cache.ts`).
- On refresh failure, keep showing cache and surface a non-blocking error.
- Never store access tokens, refresh tokens, or session secrets in local UI cache.
- Cache keys scoped to the signed-in user id.

## Account row (align lincy Proxy page)

- Status dot: active / standby / benched / unusable  
- Progress bars per usage window (5h, Week, …)  
- Promote / remove  
- Add account → provider-specific login UI  

## Models page

- **Live data only** (no hard-coded catalog):
  - Claude Code: upstream `GET /v1/models` with OAuth
  - Grok: upstream `GET /v1/models` with OAuth
  - Codex: no public models list API → empty + message
- Only providers with a bound usable account are queried
- Session API: `GET /api/models` (`?refresh=true` bypasses 90s KV cache)
- Client: `GET /openai/v1/models` (same live list); Anthropic `GET /anthropic/v1/models` (claude only, bare ids)
- Copy model id as `provider/upstream` for OpenAI clients

## Keys page

Show:

```text
OpenAI base:  https://<your-domain>/openai/v1
Anthropic:    https://<your-domain>/anthropic
```

Bases are derived from the current deploy host (`VITE_API_ORIGIN` locally, same-origin in production) — not hard-coded.
