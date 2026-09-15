//! Pool extension (apps/api/src/pool/extension.ts, docs/cloud-edition.md § "Pool
//! extension") — the only sanctioned cross-user path. Standalone installs pass none.

use std::collections::HashMap;

use async_trait::async_trait;
use http::HeaderMap;

use crate::db::AccountRow;
use crate::providers::{AccountUsage, ProviderId, UsageWindow};
use crate::routing::types::RoutingCandidate;
use crate::AppState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareInfo {
    pub team_id: String,
    pub team_name: String,
    pub owner_label: String,
}

/// One other user's builtin-provider account row, offered to a viewer.
#[derive(Debug, Clone)]
pub struct SharedAccount {
    pub account: AccountRow,
    /// The viewer's own ordering value; merged with the viewer's rows by `(priority DESC, created_at DESC)`.
    pub priority: i32,
    pub share: ShareInfo,
    /// Filled only when asked for with `usage: true`; routing never asks.
    pub usage: Option<AccountUsage>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ListSharedOptions {
    pub usage: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseOutcome {
    Consumed,
    Released,
}

/// One admitted attempt, settled exactly once at the point its `request_logs` row is decided.
#[async_trait]
pub trait AttemptLease: Send + Sync {
    async fn settle(&self, outcome: LeaseOutcome) -> anyhow::Result<()>;
}

pub enum ReserveOutcome {
    /// Not governed by the extension.
    Ungoverned,
    /// Exhausted or uncovered: move on without consuming one of the attempts.
    Skip,
    Admitted(Box<dyn AttemptLease>),
}

pub struct ReserveContext<'a> {
    pub user_id: &'a str,
    pub api_key_id: Option<&'a str>,
    pub upstream_model: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedLabel {
    pub label: String,
    pub owner_label: String,
}

#[async_trait]
pub trait PoolExtension: Send + Sync {
    /// Rows the viewer may borrow for one builtin provider. Never called for pinned targets or custom/CLI providers.
    async fn list_shared(&self, cx: &AppState, viewer_user_id: &str, provider: ProviderId, options: ListSharedOptions) -> anyhow::Result<Vec<SharedAccount>>;
    /// Every candidate attempt, own or shared.
    async fn reserve_attempt(&self, cx: &AppState, ctx: ReserveContext<'_>, candidate: &RoutingCandidate) -> anyhow::Result<ReserveOutcome>;
    /// Reorder a shared row inside the viewer's merged pool; `false` when the viewer may not (404 to the caller).
    async fn set_shared_priority(&self, cx: &AppState, viewer_user_id: &str, account_id: &str, priority: i32) -> anyhow::Result<bool>;
    /// Extra bars for the viewer's own rows of one provider (Providers page only).
    async fn own_bars(&self, _cx: &AppState, _viewer_user_id: &str, _provider: ProviderId, _account_ids: &[String]) -> anyhow::Result<HashMap<String, Vec<UsageWindow>>> {
        Ok(HashMap::new())
    }
    /// Names for borrowed accounts the viewer may still see (`GET /api/logs`).
    async fn label_shared(&self, _cx: &AppState, _viewer_user_id: &str, _account_ids: &[String]) -> anyhow::Result<HashMap<String, SharedLabel>> {
        Ok(HashMap::new())
    }
}

/// A lease must never take the response down with it.
pub async fn settle_lease(lease: Option<&dyn AttemptLease>, outcome: LeaseOutcome) {
    if let Some(lease) = lease {
        if let Err(error) = lease.settle(outcome).await {
            tracing::error!(?outcome, %error, "Failed to settle pool attempt lease");
        }
    }
}

/// A borrowed row's upstream response headers minus everything that describes the owner's
/// account: rate-limit budgets and reset times, `retry-after`, organization/account ids.
pub fn borrower_safe_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let key = name.as_str().to_ascii_lowercase();
        if key.contains("ratelimit")
            || key.contains("rate-limit")
            || key == "retry-after"
            || key.contains("organization")
            || key.contains("-org-id")
            || key.contains("account-id")
        {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_owner_headers() {
        let mut h = HeaderMap::new();
        h.insert("anthropic-ratelimit-tokens-remaining", "1".parse().unwrap());
        h.insert("x-ratelimit-reset", "1".parse().unwrap());
        h.insert("retry-after", "3".parse().unwrap());
        h.insert("anthropic-organization-id", "org".parse().unwrap());
        h.insert("x-org-id", "org".parse().unwrap());
        h.insert("anthropic-account-id", "acc".parse().unwrap());
        h.insert("x-kano-ratelimit-reset", "1".parse().unwrap());
        h.insert("content-type", "application/json".parse().unwrap());
        let out = borrower_safe_headers(&h);
        assert_eq!(out.len(), 1);
        assert!(out.contains_key("content-type"));
    }
}
