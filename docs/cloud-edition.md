# Cloud edition and reusable core

## Contract

This repository is the open-source product and stays independently runnable. The private repository, kano-proxy-cloud, pins it as a Git submodule, composes the API and web through the entry points below, and owns subscriptions, request allowance accounting, teams and the official deployment. Never commit commercial implementation to this repository.

This repository verifies, releases and distributes the CLI; it has no production deploy workflow, and a public Release deploys nothing. Subscription, allowance and team policy is specified in the private repository.

## Module boundaries

- The core crate exports a router factory, configuration types and explicit extension traits. Its standalone binary is the reference self-hosted deployment.
- The web app exports a bootstrap/router factory and explicit route, navigation and account-menu extension contracts. Its own entry point is the standalone web app.
- Extensions are passed at composition time; no global mutable plugin registry. Constructing two apps must not leak routes, middleware, or policies between them.
- Authentication establishes the caller. Subscription policy runs after authentication without replacing the caller identity. Credentials are never returned to clients.
- Personal pools are isolated by default. Cross-user account access exists only through the explicit pool extension below; unrestricted cross-tenant lookup is forbidden. Teams, sharing, member roles and budgets are edition features built on it; the core carries no team concept.
- An edition that must change its own rows and an API key together passes its transaction to the core's key functions (`create_key`, `create_key_for_device`, `delete_key` accept any Postgres executor, [auth.md](./auth.md) § Client API keys). The key commits or rolls back with the edition's rows.
- Core migrations are immutable. Edition migrations have distinct names and their own history table, and apply after the core's ([database.md](./database.md) § Migrations).

## Pool extension (composition-time, optional)

An edition passes a `PoolExtension` (`pool/extension.rs`) with `AppStateBuilder::pool_extension`. Standalone installs pass none, and nothing is ever shared. The extension is the only sanctioned cross-user path; the core never looks up another user's rows on its own.

```rust
pub struct ShareInfo { pub team_id: String, pub team_name: String, pub owner_label: String }
pub struct SharedAccount {
    pub account: AccountRow,          // another user's upstream_accounts row of a builtin provider
    pub priority: i32,                // the viewer's own ordering; merged with the viewer's rows by (priority DESC, created_at DESC)
    pub share: ShareInfo,
    pub usage: Option<AccountUsage>,  // only when asked with `usage: true` (the Providers page); routing never asks
}
pub struct SharedCustomProvider {
    pub provider: CustomProviderRow,  // another user's custom_providers row
    pub account: AccountRow,          // that endpoint's one upstream_accounts row (the owner's user_id)
    pub share: ShareInfo,
    pub usage: Option<AccountUsage>,  // as above
}
pub enum LeaseOutcome { Consumed, Released }
pub trait AttemptLease { async fn settle(&self, outcome: LeaseOutcome) -> anyhow::Result<()>; }
pub enum ReserveOutcome {
    Ungoverned,                       // not governed by the extension
    Skip,                             // exhausted or uncovered: move on without using an attempt
    Admitted(Box<dyn AttemptLease>),
}

pub trait PoolExtension {
    async fn list_shared(&self, cx: &AppState, viewer_user_id: &str, provider: ProviderId, options: ListSharedOptions) -> anyhow::Result<Vec<SharedAccount>>;
    /// Custom endpoints shared with the viewer, every slug at most once. Default: none.
    async fn list_shared_custom(&self, cx: &AppState, viewer_user_id: &str, options: ListSharedOptions) -> anyhow::Result<Vec<SharedCustomProvider>>;
    /// Every candidate attempt, own or shared.
    async fn reserve_attempt(&self, cx: &AppState, ctx: ReserveContext<'_>, candidate: &RoutingCandidate) -> anyhow::Result<ReserveOutcome>;
    /// Reorder a shared row inside the viewer's pool; `false` when the viewer may not (the order route answers 409, the pool changed).
    async fn set_shared_priority(&self, cx: &AppState, viewer_user_id: &str, account_id: &str, priority: i32) -> anyhow::Result<bool>;
    /// Extra bars for the viewer's own rows of one provider (Providers page only). Default: none.
    async fn own_bars(&self, cx: &AppState, viewer_user_id: &str, provider: ProviderId, account_ids: &[String]) -> anyhow::Result<HashMap<String, Vec<UsageWindow>>>;
    /// Names for borrowed accounts the viewer may still see (`GET /api/logs`). Default: none.
    async fn label_shared(&self, cx: &AppState, viewer_user_id: &str, account_ids: &[String]) -> anyhow::Result<HashMap<String, SharedLabel>>;
}
```

`ReserveContext` carries `user_id` and `upstream_model`; `ListSharedOptions` carries `usage`; `AccountUsage` is `{ windows, show_credits }`; `SharedLabel` is `{ label, owner_label }`.

Core obligations:

- `routing/candidates.rs` merges `list_shared` rows into unpinned builtin-pool candidates, and `GET /api/providers/:provider/accounts` lists them, in the same `(priority DESC, created_at DESC)` order (with the `share` descriptor; the core never includes the owner's upstream usage, but the route asks `list_shared` with `usage: true` so the edition may attach its own `usage.windows` for the viewer, e.g. their remaining allowance, or the account's stored windows when its owner opted into showing them; with those, `usage.show_credits` asks the route to add a Codex account's stored `credits` facts as `usage.credits` and to report `on_credits` for the row, both read off the row's own stored facts so the edition never copies them — routing never asks) and to the catalog's bound-provider set. A pinned builtin group target whose id is not one of the viewer's own rows resolves through the same `list_shared` list (validated at write time, re-resolved per request), so a viewer may order a teammate's account against their own; a row that stops being shared leaves the pin unresolved ([providers.md](./providers.md) § Model groups "Account pinning").
- **Shared custom endpoints.** A model prefix resolves builtin → the caller's own custom slug → the caller's own CLI slug → a slug in `list_shared_custom` ([providers.md](./providers.md) § Custom endpoints "Shared endpoints"). The caller's own provider always wins, so a shared endpoint whose slug the viewer already uses is unreachable for them and is reported as `shadowed` wherever it is listed. A shared endpoint contributes exactly one candidate: the owner's `custom_providers` row builds the adapter, the owner's account row carries the key and the request-edit switches, and the candidate carries the `share` descriptor like a borrowed builtin row, so `reserve_attempt`, lease settlement, owner-scoped bench writes and the borrower-safe response headers apply unchanged. The same list feeds the catalog (one section per shared endpoint, fetched with the shared key, cached under the **owner's** user id and slug so owner and borrowers read one entry), `GET /api/custom-providers` (a separate `shared` array: name, slug, format, status, `share`, the edition's `usage`, `shadowed`, the owner's request-edit switches; never `base_url`, `count_tokens_url` or the key mask), the group-target validator (an unpinned target may name a shared slug; a pin never names a shared endpoint's key row, since the endpoint is one key and the unpinned target already means exactly it) and the Overview's live-provider set. Every write route looks a custom provider up by `(id, user_id)`, so a borrower's write on a shared endpoint is `404`. A `list_shared_custom` error is **propagated** where the shared endpoint may be the whole answer — request routing (direct and group targets, which ask at most once per request or per `GET /api/model-groups`, and only after a prefix missed the caller's own namespace) and the group-target validator (which asks only when a target's prefix missed the caller's own namespace, so a group of the caller's own providers never depends on the extension) — so the caller gets the same retryable `500` a storage error gives, never a misleading `400 invalid_model`; the display readers (`GET /api/custom-providers`, the catalog, the Overview) log it and read "nothing shared". A borrowed custom endpoint's response headers are reduced to `content-type` on every delivery path (an arbitrary gateway's headers can locate or identify it; the transport already decodes `content-encoding`), while a borrowed builtin row keeps the denylist above. A shared section's catalog error reaches a borrower as the fixed `models unavailable`, never the adapter's text (a connect error names the owner's base URL); the owner's own view of the same cache entry keeps the real text. CLI providers are never shared: a tunnel ends on one person's machine.
- `dispatch_walk` calls `reserve_attempt` immediately before each attempt's acquire; a `Skip` is not counted toward `MAX_ATTEMPTS`. A lease is settled exactly once, at the point the core already decides that attempt's `request_logs` row: `released` when the attempt was benched/retried, when a non-stream upstream error status is passed through, or when the row carries an `error_code` and no upstream bytes reached the client; `consumed` otherwise. "Output" is byte-level on purpose: the core does not parse protocol events to decide a lease, so an edition needing a protocol-aware rule applies it in its own `request_policy`. For streams that is the stream-close write, so a lease may outlive the returned `Response`.
- Unpause/delete/patch on a shared row from a non-owner return 403. The ordering write is the exception: `PUT …/accounts/order` delegates a shared row to `set_shared_priority`, which orders it inside the viewer's own pool only.
- Credentials of shared accounts are decrypted only inside dispatch, as for own accounts, and never returned by any route.
- A shared candidate's request edits ([providers.md](./providers.md) § Request edits) are the owner's switches on the row `list_shared` handed over, and only those: the borrower's own accounts and settings never touch a lent account. The row is the owner's `upstream_accounts` row, so the switches ride on it and the extension needs nothing new. `GET …/accounts` reports them on the shared row as `strip_fast_mode` / `strip_claude_code_line` / `force_cache_ttl_1h`, and a borrower's `PATCH` on that row answers 403 like the other owner-only writes.
- A borrowed row's upstream response headers are stripped of everything that states the **owner's** account rather than this request — rate-limit budgets and resets (`anthropic-ratelimit-*`, `x-ratelimit-*`, the internal `x-kano-ratelimit-reset`), `retry-after`, and any organization/account identifier — on every delivery path (non-stream passthrough, streamed passthrough, audio). Own rows and standalone installs pass headers unchanged.

The optional `label_shared` lets the Logs page name a borrowed account: `routes/logs.rs` collects the page's `account_id`s that are not the viewer's own rows and asks the extension once; an id it returns renders as the label with a Shared tag and `account_shared_by` set to its `owner_label`, an id it omits falls back to the label stored on the row at write time with the removed tag ([admin-ui.md](./admin-ui.md) § Logs page). The edition decides what "may still see" means — the cloud edition answers only for accounts currently shared with the viewer's team.

The optional `own_bars` lets the edition put bars before the upstream windows on the viewer's own rows on the Providers page (their allowance on an account they lend out); those bars are added after the routing dot was decided and never reach `routing/facts`.

Teams themselves (membership, limits, ledgers, UI) stay in the private edition.

## Web shell (composition-time, optional)

`createWebApp` takes `shell` beside `routes`, `navigation`, `accountMenu` and `settingsSections`:

```ts
export interface ShellOptions {
  changelog?: boolean                 // false: no Changelog link, no version badge, no /changelog route, no /api/changelog request
  docs?: "sidebar" | "accountMenu"    // where the Documentation link lives; "sidebar" is the standalone default
}
```

Both exist for a hosted edition whose release notes are written for its operator, not for the people signed in to it ([admin-ui.md](./admin-ui.md) § Layout: the shell, "Edition options"). Hiding the page does not unmount `/api/changelog`: an edition that hides it lists that path in `Extensions::shadowed_paths` and answers it itself, or leaves `GITHUB_REPO` unset so the core's route reports unavailable.

`settingsSections` adds sections to the Settings dialog after the core's General and Account ([admin-ui.md](./admin-ui.md) § Settings dialog):

```ts
export interface SettingsSection {
  name: string          // the `?settings=` value; a core name is ignored
  label: string
  icon: Component
  component: Component  // renders the section's groups; the dialog draws its heading
  clearQuery?: readonly string[] // one-shot query keys this section reads, dropped when the dialog closes
}
```

An edition that moves an account page into Settings keeps its old path as a redirect to `/overview?settings=<name>`, carrying the rest of the query, so bookmarks and payment returns still land on it. `SettingsGroup` and `SettingsRow` are exported for its sections to draw with.

Extension routes passed in `routes` read two `meta` flags the same way core routes do:

- `bare: true` renders the page outside the shell, with no sidebar, header or nav, like the CLI authorize view ([admin-ui.md](./admin-ui.md) § CLI authorize view). The route stays session-gated unless it is also `public`.
- `restorable: false` keeps the path out of the persisted last route, so a reopened tab never lands on it ([admin-ui.md](./admin-ui.md) § View preferences). Use it for a page that only makes sense with its one-shot query, such as an authorize page.

Two more fields shape what a signed-out visitor meets. The standalone build sets neither, so its visitors go straight to sign-in.

- `landing` is the path of a `public` route the edition passes in `routes`: its landing page. A signed-out visitor who opens the bare root lands there instead of on the sign-in page, and the sign-in page's logo links to it. Any other path is a deep link and still goes through sign-in, and a signed-in visitor on the root still lands in the app. Mark the route `restorable: false` as well, or a signed-in user who opened it would be returned there on the next visit ([admin-ui.md](./admin-ui.md) § View preferences).
- `loginLinks` adds `{ label, href }` links to the sign-in page's footer after Documentation, such as the edition's terms and privacy notice. They are plain anchors, so an `href` may leave the app.

`noticeTerms` takes the same `{ label, href }` shape: the documents a user agrees to on the usage notice page ([admin-ui.md](./admin-ui.md) § Usage notice page), such as the edition's Terms of Service and Privacy Notice. With them the checkbox reads "I have read this notice and agree to the …" with the links; without them, "I have read this notice." The links open in a new tab, so the page and its ticked box stay.

## Usage notice (composition-time, optional)

The core asks every signed-in user to accept its usage notice before any session route answers ([auth.md](./auth.md) § Usage notice). An edition whose own terms the user accepts on the same page passes their date, `YYYY-MM-DD`, with `AppStateBuilder::terms_date` (any other shape panics at `build`). The stored version becomes `<notice date>+<terms date>`, so a new date on either side asks everyone again. The edition's routes that take `SessionUser` are gated like the core's; a page that must answer before acceptance takes `MaybeSessionUser`. A route that loads the session itself (`load_session_user`) is not gated: it checks `state.notice().accepted_by(&user)` and answers `403 {"error":"notice_required"}` until the user accepts. The edition changes the date in the same release as its terms text.

The public documentation site is extended the same way, from the edition's own VitePress project ([docs-site.md](./docs-site.md) § Editions).
