//! The candidate walk (docs/providers.md § Routing module,
//! docs/cloud-edition.md § "Pool extension").
//!
//! The one candidate walk every dispatch surface shares. It plans the candidate list, then for
//! each candidate: reserve the attempt with the pool extension, acquire (decrypt + refresh),
//! call the adapter under the first-byte timeout with a single 529 retry, ask
//! [`crate::routing::feedback`] whether the response is a bench-type failure, and either move
//! on or hand the response back. It never shapes the client response and never branches on
//! protocol — the transports in [`crate::proxy::dispatch`] turn a [`WalkOutcome`] into HTTP or
//! SSE frames.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::response::Response;
use rand::Rng;

use crate::app::now_ms;
use crate::db::accounts::record_edge_timeout_strike;
use crate::pool::acquire::{acquire, AcquiredAccount};
use crate::pool::bench::{mark_benched, mark_scope_benched};
use crate::pool::extension::{settle_lease, AttemptLease, LeaseOutcome, ReserveContext, ReserveOutcome};
use crate::pool::sign_in::{clear_sign_in_failure, mark_sign_in_rejected, read_refresh_failure};
use crate::providers::types::{AdapterError, CallExtras, RequestEdits};
use crate::providers::ProviderId;
use crate::routing::facts::{candidate_facts_list, earliest_unusable_until};
use crate::routing::feedback::{
    agent_fault_verdict, is_edge_timeout_status, is_overloaded_status, penalty_for_outcome, EDGE_TIMEOUT_COOLDOWN_MS,
};
use crate::routing::strategy::order_candidates;
use crate::routing::types::RoutingCandidate;
use crate::AppState;

/// The candidate walk never makes more than this many real upstream calls in one request.
pub const MAX_ATTEMPTS: usize = 8;

/// `cancelled()` for the eager transport: the client already went away.
pub type CancelSignal = Arc<dyn Fn() -> bool + Send + Sync>;

enum Plan {
    NoAccount,
    Unavailable { until_ms: Option<i64> },
    Usable { ordered: Vec<RoutingCandidate> },
}

fn plan_candidates(candidates: Vec<RoutingCandidate>) -> Plan {
    if candidates.is_empty() {
        return Plan::NoAccount;
    }
    let now = now_ms();
    let facts = candidate_facts_list(&candidates, now);
    let ordered = order_candidates(candidates, &facts);
    let usable: Vec<RoutingCandidate> =
        ordered.into_iter().filter(|o| o.facts.usable).map(|o| o.candidate).collect();
    if usable.is_empty() {
        return Plan::Unavailable { until_ms: earliest_unusable_until(&facts) };
    }
    Plan::Usable { ordered: usable }
}

/// Decrypt + `refresh_if_needed` for one candidate — `None` on an unreadable credential
/// (skipped, never counted as an attempt).
async fn acquire_candidate(cx: &AppState, candidate: &RoutingCandidate) -> Option<AcquiredAccount> {
    let acquired = match acquire(cx, candidate.account.clone()) {
        Ok(acquired) => acquired,
        Err(error) => {
            tracing::debug!(account_id = %candidate.account.id, %error, "candidate credential is unreadable");
            return None;
        }
    };
    match candidate.adapter.refresh_if_needed(cx, acquired).await {
        Ok(acquired) => Some(acquired),
        Err(error) => {
            tracing::debug!(account_id = %candidate.account.id, %error, "candidate refresh failed");
            None
        }
    }
}

/// Per-attempt upstream response-header deadline (docs/api.md § Keepalive and idle timeout).
fn first_byte_timeout(cx: &AppState) -> Duration {
    Duration::from_millis(cx.config().upstream_first_byte_timeout_ms)
}

enum Fetched {
    Response(Response),
    /// No response headers inside the deadline — fail over without benching.
    TimedOut,
    /// The adapter itself failed (network, adapter bug) — terminal for this request.
    Failed,
}

/// One candidate's upstream call, already bound to the request body/headers; the walk supplies
/// the acquired credential and the per-attempt deadline.
#[async_trait]
pub trait CandidateCaller: Send + Sync {
    /// `false` skips a candidate whose adapter lacks the endpoint (never counted as an attempt).
    fn supports(&self, candidate: &RoutingCandidate) -> bool;

    async fn call(
        &self,
        cx: &AppState,
        candidate: &RoutingCandidate,
        acquired: &AcquiredAccount,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError>;
}

/// Waits only for the adapter to produce response headers. The deadline also reaches the
/// adapter through `CallExtras::first_byte_timeout`, so the transport aborts the request
/// itself; the outer timeout covers an adapter that never returns at all.
async fn fetch_upstream_response(
    cx: &AppState,
    caller: &dyn CandidateCaller,
    candidate: &RoutingCandidate,
    acquired: &AcquiredAccount,
    extras: &CallExtras,
) -> Fetched {
    match tokio::time::timeout(first_byte_timeout(cx), caller.call(cx, candidate, acquired, extras)).await {
        Err(_) => Fetched::TimedOut,
        Ok(Ok(response)) => Fetched::Response(response),
        Ok(Err(AdapterError::Transport(crate::upstream::TransportError::Timeout))) => Fetched::TimedOut,
        Ok(Err(error)) => {
            tracing::debug!(provider = %candidate.provider, %error, "upstream call failed");
            Fetched::Failed
        }
    }
}

/// Jitter keeps concurrent overloaded calls from immediately synchronizing.
async fn wait_for_overload_retry() {
    let jitter = rand::thread_rng().gen_range(0..200);
    tokio::time::sleep(Duration::from_millis(900 + jitter)).await;
}

/// A 529 is transient fleet overload, so retry this account exactly once without benching it.
async fn retry_529_once(
    cx: &AppState,
    caller: &dyn CandidateCaller,
    candidate: &RoutingCandidate,
    acquired: &AcquiredAccount,
    extras: &CallExtras,
) -> Fetched {
    let result = fetch_upstream_response(cx, caller, candidate, acquired, extras).await;
    match &result {
        Fetched::Response(res) if res.status().as_u16() == 529 => {}
        _ => return result,
    }
    drop(result);
    wait_for_overload_retry().await;
    fetch_upstream_response(cx, caller, candidate, acquired, extras).await
}

/// How long a `401` waits for a refresh that is in flight on the account before it is taken at
/// face value. The refresh was already under way when upstream refused the old token, so what
/// is left is the winner storing its answer: one second covers it, and bounds what a lock left
/// behind by a killed process can cost a `401` that was real.
const SUPERSEDED_POLL_ATTEMPTS: u32 = 10;
const SUPERSEDED_POLL_DELAY: Duration = Duration::from_millis(100);

/// The credential now stored for `acquired`'s account, when it is no longer the one this attempt
/// sent. Waits out a refresh in flight first: the request that loses the refresh race is sent
/// with the old token and is refused before the winner has stored the new one. A credential
/// with no refresh token is a key: nothing replaces it, so its `401` costs no lookup. Gives up
/// as soon as the client has gone away.
async fn superseding_credential(
    cx: &AppState,
    acquired: &AcquiredAccount,
    cancelled: Option<&CancelSignal>,
) -> Option<AcquiredAccount> {
    acquired.credential.refresh_token.as_ref()?;
    for attempt in 0..=SUPERSEDED_POLL_ATTEMPTS {
        if cancelled.is_some_and(|gone| gone()) {
            return None;
        }
        let row = crate::db::accounts::get_account(cx.pool(), &acquired.row.user_id, &acquired.row.id).await.ok()??;
        if row.encrypted_payload != acquired.row.encrypted_payload {
            return acquire(cx, row).ok();
        }
        // No refresh in flight, and the stored credential is the one that was refused.
        row.refreshing_at.as_ref()?;
        if attempt < SUPERSEDED_POLL_ATTEMPTS {
            tokio::time::sleep(SUPERSEDED_POLL_DELAY).await;
        }
    }
    None
}

/// One candidate's call, with a `401` for a superseded credential retried on the stored one
/// (docs/providers.md § Routing module "Penalties"). An OAuth refresh can revoke the access
/// token it replaces, so a request already holding the old one is refused although the sign-in
/// is fine; benching for that turned every refresh under load into a five-minute outage (observed
/// 2026-11-07). The retry happens once, before any bytes were piped, and its answer is judged
/// like any other. Callers that merely lost the refresh race now wait for the winner inside the
/// refresh itself (`providers::refresh`); this is for a request that was already on the wire.
async fn fetch_with_stored_credential(
    cx: &AppState,
    caller: &dyn CandidateCaller,
    candidate: &RoutingCandidate,
    acquired: AcquiredAccount,
    extras: &CallExtras,
    cancelled: Option<&CancelSignal>,
) -> (AcquiredAccount, Fetched) {
    let fetched = retry_529_once(cx, caller, candidate, &acquired, extras).await;
    let unauthorized = matches!(&fetched, Fetched::Response(res) if res.status().as_u16() == 401);
    if !unauthorized {
        return (acquired, fetched);
    }
    // The refused response is kept, owned, across the lookup: when nothing replaced the
    // credential it is the answer the walk judges.
    let Some(current) = superseding_credential(cx, &acquired, cancelled).await else {
        return (acquired, fetched);
    };
    if cancelled.is_some_and(|gone| gone()) {
        // Nobody is left to answer: the walk's own check drops this response, unsent again.
        return (acquired, fetched);
    }
    drop(fetched);
    let fetched = match retry_529_once(cx, caller, candidate, &current, extras).await {
        // The refused answer would have moved the walk on to the next candidate. A retry that
        // got no answer must not end the request in its place: skip this candidate, unbenched.
        Fetched::Failed => Fetched::TimedOut,
        answered => answered,
    };
    (current, fetched)
}

/// Bench persistence is feedback, never a reason to abort an in-flight failover walk. Bench
/// writes are scoped to the row's **owner** (`account.user_id`), which is the caller for every
/// own row and the sharer for a borrowed one — a shared account that just failed must really
/// get benched, not silently no-op.
async fn persist_bench(cx: &AppState, candidate: &RoutingCandidate, cooldown_ms: i64, reason: &str) {
    if let Err(error) = mark_benched(
        cx.pool(),
        &candidate.account.user_id,
        &candidate.provider,
        &candidate.account.id,
        cooldown_ms,
        reason,
        now_ms(),
    )
    .await
    {
        tracing::error!(
            account_id = %candidate.account.id,
            provider = %candidate.provider,
            %error,
            "Failed to persist account bench"
        );
    }
}

/// [`persist_bench`] for one quota scope of the account (docs/providers.md § Routing module
/// "Quota scopes") — same owner scoping, same non-fatal failure.
async fn persist_scope_bench(cx: &AppState, candidate: &RoutingCandidate, scope: &str, cooldown_ms: i64, reason: &str) {
    if let Err(error) = mark_scope_benched(
        cx.pool(),
        &candidate.account.user_id,
        &candidate.provider,
        &candidate.account.id,
        scope,
        cooldown_ms,
        reason,
        now_ms(),
    )
    .await
    {
        tracing::error!(
            account_id = %candidate.account.id,
            provider = %candidate.provider,
            scope,
            %error,
            "Failed to persist scoped bench"
        );
    }
}

/// An upstream `401` on a builtin account is the stored sign-in being refused, which outlives
/// the bench written beside it (docs/providers.md § Sign-in state). Non-fatal like the bench, and
/// scoped to the row's owner the same way.
async fn persist_sign_in_rejected(cx: &AppState, candidate: &RoutingCandidate) {
    if ProviderId::parse(&candidate.provider).is_none() {
        return;
    }
    if let Err(error) = mark_sign_in_rejected(
        cx.pool(),
        &candidate.account.user_id,
        &candidate.provider,
        &candidate.account.id,
        now_ms(),
    )
    .await
    {
        tracing::error!(account_id = %candidate.account.id, provider = %candidate.provider, %error, "Failed to persist sign-in rejection");
    }
}

/// A `2xx` proves upstream accepts the credential, so a recorded refusal is forgotten, and with
/// it a failed refresh whose record says the token had run out. Read off the row this attempt
/// actually used: a refresh that just succeeded already cleared both. A refresh refused while
/// the old token still works is left alone: the old token working does not disprove it, and its
/// record is what paces the retry and carries the notice.
async fn forget_expired_sign_in(cx: &AppState, acquired: &AcquiredAccount) {
    let row = &acquired.row;
    let ran_out = read_refresh_failure(row).is_some_and(|failure| failure.token_ran_out(now_ms()));
    if row.sign_in_rejected_at.is_none() && !ran_out {
        return;
    }
    if let Err(error) = clear_sign_in_failure(cx.pool(), &row.user_id, &row.id, ran_out).await {
        tracing::error!(account_id = %acquired.row.id, %error, "Failed to clear sign-in failure");
    }
}

/// Edge-timeout feedback is non-fatal just like bench persistence. Every edge status remains
/// excluded from this walk; only the atomic third strike earns a bench. `false` therefore
/// covers strikes 1–2 and a failed write.
async fn persist_edge_timeout_strike(cx: &AppState, candidate: &RoutingCandidate) -> bool {
    match record_edge_timeout_strike(
        cx.pool(),
        &candidate.account.user_id,
        &candidate.provider,
        &candidate.account.id,
        now_ms(),
    )
    .await
    {
        Ok(third) => third,
        Err(error) => {
            tracing::error!(
                account_id = %candidate.account.id,
                provider = %candidate.provider,
                %error,
                "Failed to persist edge-timeout strike"
            );
            false
        }
    }
}

/// Apply feedback and decide whether this pre-stream response must fail over.
/// `status` and `headers` rather than the whole response: an `axum::Response` is not `Sync`,
/// and this runs inside the eager transport's `Send` producer future.
async fn should_fail_over_for_response(
    cx: &AppState,
    candidate: &RoutingCandidate,
    status: u16,
    headers: &http::HeaderMap,
) -> bool {
    // Agent-tunnel fault (docs/cli.md § Failover semantics): infrastructure failure between the
    // tunnel and the CLI — always the next candidate; only `offline` benches (60s), and
    // reconnect clears that bench.
    if let Some(fault) = agent_fault_verdict(headers) {
        if let Some(bench_ms) = fault.bench_ms {
            persist_bench(cx, candidate, bench_ms, &fault.reason).await;
        }
        return true;
    }
    if is_edge_timeout_status(status) {
        if persist_edge_timeout_strike(cx, candidate).await {
            persist_bench(cx, candidate, EDGE_TIMEOUT_COOLDOWN_MS, &status.to_string()).await;
        }
        return true;
    }
    let scope = candidate.adapter.quota_scope(&candidate.upstream_model);
    let Some(penalty) = penalty_for_outcome(status, headers, &candidate.account, scope, now_ms()) else {
        return false;
    };
    match penalty.scope {
        Some(scope) => persist_scope_bench(cx, candidate, scope, penalty.cooldown_ms, &status.to_string()).await,
        None => persist_bench(cx, candidate, penalty.cooldown_ms, &status.to_string()).await,
    }
    if status == 401 {
        persist_sign_in_rejected(cx, candidate).await;
    }
    true
}

/// Recomputes the earliest bench/limit expiry across exactly the candidates this walk just
/// tried and benched — the exclude set behind the bottom-of-loop `Retry-After`, expressed as a
/// fresh facts read (cheap: bounded by [`MAX_ATTEMPTS`], and correct even though a 429's
/// cooldown can vary per candidate).
pub async fn recompute_unavailable_until(cx: &AppState, tried: &[RoutingCandidate]) -> Option<i64> {
    if tried.is_empty() {
        return None;
    }
    let now = now_ms();
    let mut earliest: Option<i64> = None;
    for candidate in tried {
        // The rows were mutated by this walk's bench writes, so they are re-read rather than
        // re-using the copies the plan captured.
        let row = match crate::db::accounts::get_account(cx.pool(), &candidate.account.user_id, &candidate.account.id)
            .await
        {
            Ok(Some(row)) => row,
            Ok(None) => continue,
            Err(error) => {
                tracing::error!(%error, "Failed to re-read a tried candidate for Retry-After");
                continue;
            }
        };
        let scope = candidate.adapter.quota_scope(&candidate.upstream_model);
        if let Some(until) = crate::routing::facts::facts_for_row(&row, scope, now).unusable_until {
            earliest = Some(earliest.map_or(until, |e: i64| e.min(until)));
        }
    }
    earliest
}

/// The attempt record: what the transports need to log, no matter how the walk ended.
#[derive(Default)]
pub struct WalkProgress {
    /// Last candidate a real upstream call was made for (`None` before the first attempt).
    pub candidate: Option<RoutingCandidate>,
    /// HTTP status of the last upstream response seen, bench-type or not.
    pub upstream_status: Option<i32>,
}

pub struct WalkOpts<'a> {
    /// The requesting user and API key, for the pool extension's admission and `CallExtras`.
    pub user_id: &'a str,
    pub api_key_id: Option<&'a str>,
    /// What dispatch walks: the candidate list the route resolved (docs/providers.md § Routing
    /// module) — one provider's pool, or a group's cross-target list.
    pub candidates: Vec<RoutingCandidate>,
    pub caller: &'a dyn CandidateCaller,
    /// Eager transport: the client already went away — stop before the next acquire/fetch.
    pub cancelled: Option<CancelSignal>,
    /// Filled in as the walk goes so a log written after an early exit (cancel, throw) still
    /// names the last candidate tried.
    pub progress: Arc<Mutex<WalkProgress>>,
}

pub enum WalkOutcome {
    /// Zero candidates for this target — retrying can never help (400 / `no_upstream_account`).
    NoAccount,
    /// Candidates exist but every one is benched or over its usage window right now
    /// (503 / `upstream_unavailable`, `Retry-After` from the plan).
    Unavailable { until_ms: Option<i64> },
    /// `cancelled()` fired mid-walk; any response in hand has been dropped. Never returned when
    /// `cancelled` is absent.
    Cancelled,
    /// The adapter call failed (network, adapter bug) — terminal, no further candidates
    /// (502 / `upstream_error`).
    FetchError { candidate: RoutingCandidate },
    /// Every attempt was a bench-type failure: either the list ran dry (`last_benched` = the
    /// candidate whose bench response ended it) or the attempt cap hit / the rest was
    /// undecryptable (`last_benched` `None`). Same synthesized 503 either way; `tried` is the
    /// exclude set for `Retry-After`.
    Exhausted { tried: Vec<RoutingCandidate>, last_benched: Option<RoutingCandidate> },
    /// A non-bench response — success or a terminal upstream error — for the transport to
    /// deliver. `lease` is this attempt's still-open pool-extension lease: the transport settles
    /// it exactly once, where it decides the attempt's `request_logs` row. Every other outcome
    /// has already settled its own leases `Released`.
    Response { candidate: RoutingCandidate, response: Response, lease: Option<Box<dyn AttemptLease>> },
}

pub async fn walk_candidates(cx: &AppState, opts: WalkOpts<'_>) -> WalkOutcome {
    let cancelled = || opts.cancelled.as_ref().is_some_and(|f| f());
    let (user_id, api_key_id) = (opts.user_id, opts.api_key_id);
    let plan = plan_candidates(opts.candidates);
    if cancelled() {
        return WalkOutcome::Cancelled;
    }
    let ordered = match plan {
        Plan::NoAccount => return WalkOutcome::NoAccount,
        Plan::Unavailable { until_ms } => return WalkOutcome::Unavailable { until_ms },
        Plan::Usable { ordered } => ordered,
    };

    let base_extras = CallExtras {
        api_key_id: api_key_id.map(str::to_owned),
        first_byte_timeout: Some(first_byte_timeout(cx)),
        edits: Default::default(),
    };
    // The candidate whose bench-type response ended the list, if one did.
    let mut last_benched: Option<RoutingCandidate> = None;
    let mut idx = 0usize;
    let mut attempts = 0usize;
    let mut ran_out_of_candidates = false;
    while attempts < MAX_ATTEMPTS {
        if cancelled() {
            return WalkOutcome::Cancelled;
        }
        let Some(candidate) = ordered.get(idx).cloned() else {
            ran_out_of_candidates = true;
            break;
        };
        // The candidate's own account switches (docs/providers.md § Request edits); on a shared
        // row the account is the owner's, so these are the owner's switches.
        let edits = RequestEdits {
            strip_fast_mode: candidate.account.strip_fast_mode,
            strip_claude_code_line: candidate.account.strip_claude_code_line,
            force_cache_ttl_1h: candidate.account.force_cache_ttl_1h,
        };
        let extras = CallExtras { edits, ..base_extras.clone() };
        idx += 1;
        if !opts.caller.supports(&candidate) {
            continue;
        }

        // Pool extension (docs/cloud-edition.md § "Pool extension"): every attempt, own row or
        // shared, is offered for admission immediately before the acquire. A `Skip` is
        // exhaustion of that row's governed budget — move on without spending one of
        // MAX_ATTEMPTS on it.
        let mut lease: Option<Box<dyn AttemptLease>> = None;
        if let Some(ext) = cx.pool_extension() {
            let reserved = ext
                .reserve_attempt(
                    cx,
                    ReserveContext { user_id, upstream_model: &candidate.upstream_model },
                    &candidate,
                )
                .await;
            match reserved {
                Ok(ReserveOutcome::Skip) => continue,
                Ok(ReserveOutcome::Admitted(l)) => lease = Some(l),
                Ok(ReserveOutcome::Ungoverned) => {}
                Err(error) => {
                    tracing::error!(%error, "Pool extension reserveAttempt failed");
                    continue;
                }
            }
        }

        let acquired = acquire_candidate(cx, &candidate).await;
        if cancelled() {
            settle_lease(lease.as_deref(), LeaseOutcome::Released).await;
            return WalkOutcome::Cancelled;
        }
        let Some(acquired) = acquired else {
            settle_lease(lease.as_deref(), LeaseOutcome::Released).await;
            continue;
        };
        attempts += 1;
        opts.progress.lock().unwrap_or_else(|e| e.into_inner()).candidate = Some(candidate.clone());

        let (acquired, fetched) = fetch_with_stored_credential(cx, opts.caller, &candidate, acquired, &extras, opts.cancelled.as_ref()).await;
        let response = match fetched {
            Fetched::Failed => {
                // Terminal for this request and logged as `upstream_error` with no output, so
                // the attempt is released, not consumed.
                settle_lease(lease.as_deref(), LeaseOutcome::Released).await;
                return WalkOutcome::FetchError { candidate };
            }
            Fetched::TimedOut => {
                settle_lease(lease.as_deref(), LeaseOutcome::Released).await;
                continue;
            }
            Fetched::Response(response) => response,
        };
        opts.progress.lock().unwrap_or_else(|e| e.into_inner()).upstream_status =
            Some(response.status().as_u16() as i32);
        crate::providers::usage_refresh::spawn_account_usage_refresh(
            cx,
            candidate.account.clone(),
            candidate.adapter.clone(),
        );
        if cancelled() {
            settle_lease(lease.as_deref(), LeaseOutcome::Released).await;
            return WalkOutcome::Cancelled;
        }

        let status = response.status().as_u16();
        // A 529 that survived the same-account retry says nothing about this account, so it is
        // never benched — but another candidate may sit on a different fleet entirely, so the
        // walk moves on while it has somewhere to go. The last candidate's 529 passes through
        // so a one-account pool keeps the honest upstream status.
        if is_overloaded_status(status) && ordered[idx..].iter().any(|later| opts.caller.supports(later)) {
            settle_lease(lease.as_deref(), LeaseOutcome::Released).await;
            drop(response);
            continue;
        }
        if should_fail_over_for_response(cx, &candidate, status, response.headers()).await {
            // Benched / edge-timed-out / retried: this attempt produced nothing.
            settle_lease(lease.as_deref(), LeaseOutcome::Released).await;
            drop(response);
            last_benched = Some(candidate);
            continue;
        }

        if (200..300).contains(&status) {
            forget_expired_sign_in(cx, &acquired).await;
        }
        return WalkOutcome::Response { candidate, response, lease };
    }

    WalkOutcome::Exhausted {
        tried: ordered.into_iter().take(idx).collect(),
        last_benched: if ran_out_of_candidates { last_benched } else { None },
    }
}
