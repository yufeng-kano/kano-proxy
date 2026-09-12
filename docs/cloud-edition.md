# Cloud edition and reusable core

## Contract

The existing self-hosted product remains fully open source and independently runnable. The private repository is named kano-proxy-cloud. It composes the public API and web through supported entry points, pins the public source to a Git submodule commit, and owns subscriptions and request allowance accounting. Never commit commercial implementation to this public repository.

Public releases retain verification and independent CLI distribution. The public production deploy workflow is disabled during the transition. The private repository takes over production deployment only after compatibility, migration and release checks pass. Releases remain the only production deployment trigger; no push deploys.

## Module boundaries

- Public API exports an application factory and Worker handler factory, environment/context types and explicit extension contracts. The existing entry point remains the standalone Worker and exports AgentTunnel under its existing name.
- Public web exports a bootstrap/router factory and explicit route, navigation and account-menu extension contracts. The existing entry point remains the standalone web app.
- Extensions are passed at composition time; no global mutable plugin registry. Constructing two apps must not leak routes, middleware, or policies between them.
- Authentication establishes the caller. Subscription policy runs after authentication without replacing the caller identity. Credentials are never returned to clients.
- Personal pools remain isolated by default. Cross-user account access exists only through the explicit pool extension below; unrestricted cross-tenant lookup is forbidden.
- Public migrations remain immutable. Cloud migrations have distinct names and a documented unified application order. Preserve existing user/key/session IDs, credential encryption keys and AgentTunnel identity on production cutover.

## Cloud product specification

Subscription entitlement belongs to the signed-in user. A subscription provides a request allowance per billing period. Users bring their own upstream accounts and pay upstream costs themselves.

Count one client model request once, regardless of upstream retries. Rejects, catalog reads and failures before useful output do not count. Streaming output followed by disconnect counts. Quotas require atomic reservation before dispatch and idempotent completion; crashes cannot silently restore already consumed credit.

Teams, model sharing, member roles and USD budgets are private-edition features built on the pool extension below; the core carries no team concept. Subscription cancellation preserves paid access until the paid-through date. Expiry falls back to Free without resetting earlier Free usage. Account management and export/revocation remain accessible. Configuration and personal resources are retained.

Payment setup must use the operator's real merchant account and configured price identifiers. Never invent production products, prices, customers or entitlements. Use verified webhooks, deduplication and out-of-order reconciliation. Payment credentials stay in secrets. Paddle Billing is the operator-selected external payment exception because Cloudflare has no native merchant subscription checkout. Application compute and storage remain on Cloudflare.

## Verification and cutover gates

1. Standalone public API/web retain their existing behavior; full tests, typecheck and site build pass.
2. Cloud composes the same source revision and runs independently; tests cover isolation of separately constructed apps and all protocol routes.
3. Subscription tests cover signature validation, replay, ordering, renewal, cancellation, failure, concurrent request limits and settlement.
4. Hosted policy tests cover every model request surface, user isolation, subscription UI and allowance display.
5. Cloud CI checks a clean checkout with recursive submodules and frozen dependencies; artifacts and migration plans identify both cloud and core revisions.
6. Production configuration, existing encrypted data compatibility, backup and rollback strategy are verified before release. No paid upstream test without specific approval.

## Implementation status

The reusable API/web entry points are implemented and merged. The standalone application remains supported. The public production workflow is retired: it has no Release trigger and its remaining manual job is skipped. Public CI and independent CLI releases remain enabled. Private implementation, launch checks and deployment status are maintained in the private repository; a public Release does not deploy the official website.

## Launch policy (operator update)

Every user, including existing users, starts with Free: 10,000 successful model requests per UTC calendar month. Paddle is selected for optional paid subscriptions; payment keys remain unset for the initial launch. Paid checkout is unavailable until real Paddle credentials and configured price IDs exist. Missing or expired paid access falls back to the user's existing Free period without resetting earlier Free usage.

## Pool extension (composition-time, optional)

An edition may pass `poolExtension` in `ApplicationOptions`. Standalone installs pass none and behave exactly as before. The extension is the only sanctioned cross-user path; the core never looks up another user's rows on its own.

```ts
export interface SharedAccount {
  account: AccountRow            // another user's upstream_accounts row, builtin provider only
  priority: number               // the viewer's own ordering; merged with the viewer's rows by (priority DESC, created_at DESC)
  share: { teamId: string; teamName: string; ownerLabel: string }
}
export type AttemptLease = { settle(outcome: "consumed" | "released"): Promise<void> }
export interface PoolExtension {
  listShared(env: Env, viewerUserId: string, provider: ProviderId): Promise<SharedAccount[]>
  /** Every candidate attempt, own or shared. `null` = not governed; `{ skip: true }` = exhausted, move on; a lease = admitted. */
  reserveAttempt(env: Env, ctx: { userId: string; apiKeyId: string | null; upstreamModel: string }, candidate: RoutingCandidate): Promise<AttemptLease | { skip: true } | null>
  setSharedPriority(env: Env, viewerUserId: string, accountId: string, priority: number): Promise<boolean>
}
```

Core obligations:

- `routing/candidates.ts` appends `listShared` rows to unpinned builtin-pool candidates and to `GET /api/providers/:provider/accounts` (with the `share` descriptor, usage windows omitted for non-owners) and to the catalog's bound-provider set. Pinned group targets never resolve to a shared account.
- `dispatch_walk` calls `reserveAttempt` immediately before each attempt's acquire; a `skip` is not counted toward `MAX_ATTEMPTS`. A lease is settled exactly once, at the point the core already decides that attempt's `request_logs` row: `released` when the attempt was benched/retried or the row carries an `error_code` with no completion output, `consumed` otherwise. For streams that is the stream-close write, so a lease may outlive the returned `Response`.
- Promote/unpause/delete/patch on a shared row from a non-owner return 403, except promote, which delegates to `setSharedPriority`.
- Credentials of shared accounts are decrypted only inside dispatch, as for own accounts, and never returned by any route.

Teams themselves (membership, limits, ledgers, UI) stay in the private edition.
