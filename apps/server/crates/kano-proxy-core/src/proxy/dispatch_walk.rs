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
use crate::db::custom_providers::CustomProviderRow;
use crate::pool::acquire::{acquire, AcquiredAccount};
use crate::pool::bench::mark_benched;
use crate::pool::extension::{settle_lease, AttemptLease, LeaseOutcome, ReserveContext, ReserveOutcome};
use crate::providers::types::{AdapterError, CallExtras};
use crate::providers::{DynAdapter, ProviderId};
use crate::routing::candidates::{pool_candidates, PoolTarget};
use crate::routing::facts::{candidate_facts_list, earliest_unusable_until};
use crate::routing::feedback::{
    agent_fault_verdict, is_edge_timeout_status, penalty_for_outcome, EDGE_TIMEOUT_COOLDOWN_MS,
};
use crate::routing::strategy::{normalize_strategy, order_candidates};
use crate::routing::types::{RoutingCandidate, StrategyContext};
use crate::AppState;

/// The candidate walk never makes more than this many real upstream calls in one request.
pub const MAX_ATTEMPTS: usize = 8;

/// `cancelled()` for the eager transport: the client already went away.
pub type CancelSignal = Arc<dyn Fn() -> bool + Send + Sync>;

/// The routing module's candidate list, ready for dispatch to walk. `candidates` and
/// `strategy` let a caller (a group dispatch, via the cross-target flattened list) hand in an
/// already-built, possibly cross-provider list; when absent, this builds the ordinary
/// single-pool list dispatch has always used.
///
/// Deviation from the TypeScript: the pool extension is not a field here — it comes from
/// `cx.pool_extension()`, the composition seam an edition already installed on the `AppState`.
#[derive(Clone, Default)]
pub struct CandidateSource {
    pub user_id: String,
    pub api_key_id: Option<String>,
    pub provider: String,
    /// Pre-resolved adapter for custom providers; defaults to the builtin registry.
    pub adapter: Option<DynAdapter>,
    /// Model-group account pinning: restrict acquire/failover to exactly this row.
    pub pinned_account_id: Option<String>,
    /// Pre-built cross-target candidate list (group dispatch) — bypasses the single-pool
    /// builder entirely.
    pub candidates: Option<Vec<RoutingCandidate>>,
    /// `model_groups.strategy` (candidates given) or `provider_settings.strategy` (single
    /// pool); defaults to `ordered`.
    pub strategy: Option<String>,
    pub is_builtin: Option<bool>,
    pub custom_provider: Option<CustomProviderRow>,
}

enum Plan {
    NoAccount,
    Unavailable { until_ms: Option<i64> },
    Usable { ordered: Vec<RoutingCandidate> },
}

async fn plan_candidates(cx: &AppState, src: &CandidateSource, upstream_model: &str) -> Result<Plan, sqlx::Error> {
    let candidates = match src.candidates.clone() {
        Some(candidates) => candidates,
        None => {
            let is_builtin = src.is_builtin.unwrap_or(true);
            let adapter = match src.adapter.clone() {
                Some(adapter) => adapter,
                None => match ProviderId::parse(&src.provider) {
                    Some(id) => crate::providers::registry::get_adapter(id),
                    // A non-builtin provider with no adapter cannot be called at all; an empty
                    // pool is the honest answer (`no_upstream_account`).
                    None => return Ok(Plan::NoAccount),
                },
            };
            pool_candidates(
                cx,
                &src.user_id,
                &PoolTarget {
                    provider: src.provider.clone(),
                    upstream_model: upstream_model.to_string(),
                    is_builtin,
                    custom_provider: src.custom_provider.clone(),
                    adapter,
                    account_id: src.pinned_account_id.clone(),
                },
            )
            .await?
        }
    };
    if candidates.is_empty() {
        return Ok(Plan::NoAccount);
    }
    let now = now_ms();
    let facts = candidate_facts_list(&candidates, now);
    let ordered = order_candidates(
        candidates,
        &facts,
        &StrategyContext {
            api_key_id: src.api_key_id.clone(),
            strategy: normalize_strategy(src.strategy.as_deref()),
        },
    );
    let usable: Vec<RoutingCandidate> =
        ordered.into_iter().filter(|o| o.facts.usable).map(|o| o.candidate).collect();
    if usable.is_empty() {
        return Ok(Plan::Unavailable { until_ms: earliest_unusable_until(&facts) });
    }
    Ok(Plan::Usable { ordered: usable })
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
/// itself; the outer timeout covers an adapter that never returns at all — together they are
/// the `AbortController` the TypeScript walk wrapped every attempt in.
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
        Some(cooldown_ms),
        Some(reason),
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
    let Some(penalty) = penalty_for_outcome(status, headers, &candidate.account, now_ms()) else {
        return false;
    };
    persist_bench(cx, candidate, penalty.cooldown_ms, &status.to_string()).await;
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
        if let Some(until) = crate::routing::facts::facts_for_row(&row, now).unusable_until {
            earliest = Some(earliest.map_or(until, |e: i64| e.min(until)));
        }
    }
    earliest
}

/// The attempt record: what the transports need to log, no matter how the walk ended.
#[derive(Default, Clone)]
pub struct WalkProgress {
    /// Last candidate a real upstream call was made for (`None` before the first attempt).
    pub candidate: Option<RoutingCandidate>,
    /// HTTP status of the last upstream response seen, bench-type or not.
    pub upstream_status: Option<i32>,
}

pub struct WalkOpts<'a> {
    pub source: &'a CandidateSource,
    /// Bare upstream model id used to build the single-pool candidate list when
    /// `source.candidates` is absent.
    pub upstream_model: &'a str,
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
    let plan = match plan_candidates(cx, opts.source, opts.upstream_model).await {
        Ok(plan) => plan,
        Err(error) => {
            tracing::error!(%error, "Failed to plan routing candidates");
            return WalkOutcome::NoAccount;
        }
    };
    if cancelled() {
        return WalkOutcome::Cancelled;
    }
    let ordered = match plan {
        Plan::NoAccount => return WalkOutcome::NoAccount,
        Plan::Unavailable { until_ms } => return WalkOutcome::Unavailable { until_ms },
        Plan::Usable { ordered } => ordered,
    };

    let extras =
        CallExtras { api_key_id: opts.source.api_key_id.clone(), first_byte_timeout: Some(first_byte_timeout(cx)) };
    let mut last_candidate: Option<RoutingCandidate> = None;
    let mut saw_bench_response = false;
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
                    ReserveContext {
                        user_id: &opts.source.user_id,
                        api_key_id: opts.source.api_key_id.as_deref(),
                        upstream_model: &candidate.upstream_model,
                    },
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

        let fetched = retry_529_once(cx, opts.caller, &candidate, &acquired, &extras).await;
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
        let headers = response.headers().clone();
        if should_fail_over_for_response(cx, &candidate, status, &headers).await {
            // Benched / edge-timed-out / retried: this attempt produced nothing.
            settle_lease(lease.as_deref(), LeaseOutcome::Released).await;
            drop(response);
            saw_bench_response = true;
            last_candidate = Some(candidate);
            continue;
        }

        return WalkOutcome::Response { candidate, response, lease };
    }

    WalkOutcome::Exhausted {
        tried: ordered.into_iter().take(idx).collect(),
        last_benched: if ran_out_of_candidates && saw_bench_response { last_candidate } else { None },
    }
}
