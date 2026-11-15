//! Pool extension (docs/cloud-edition.md § "Pool
//! extension") — the only sanctioned cross-user path. Standalone installs pass none.

use std::collections::HashMap;

use async_trait::async_trait;
use http::HeaderMap;

use crate::db::{AccountRow, CustomProviderRow};
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

/// One other user's custom endpoint, offered to a viewer (docs/providers.md § Custom endpoints
/// "Shared endpoints"). Addressed by the owner's slug; dispatched with the owner's row and key.
#[derive(Debug, Clone)]
pub struct SharedCustomProvider {
    /// The owner's `custom_providers` row.
    pub provider: CustomProviderRow,
    /// Its one `upstream_accounts` row; keeps the owner's `user_id`.
    pub account: AccountRow,
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
    /// Rows the viewer may borrow for one builtin provider. Never called for pinned targets;
    /// custom endpoints go through [`PoolExtension::list_shared_custom`], CLI providers are never shared.
    async fn list_shared(&self, cx: &AppState, viewer_user_id: &str, provider: ProviderId, options: ListSharedOptions) -> anyhow::Result<Vec<SharedAccount>>;
    /// Custom endpoints other users share with the viewer, every slug at most once. Consulted
    /// only after the viewer's own builtin/custom/CLI namespace missed, so an own slug shadows it.
    async fn list_shared_custom(&self, _cx: &AppState, _viewer_user_id: &str, _options: ListSharedOptions) -> anyhow::Result<Vec<SharedCustomProvider>> {
        Ok(Vec::new())
    }
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

/// [`PoolExtension::list_shared_custom`] as every core reader sees it: empty without an
/// extension, and an extension error is logged and read as "nothing shared", since borrowing
/// is an addition to the viewer's own providers and losing it must never take a request down.
pub async fn shared_custom_providers(cx: &AppState, viewer_user_id: &str, options: ListSharedOptions) -> Vec<SharedCustomProvider> {
    let Some(ext) = cx.pool_extension() else { return Vec::new() };
    match ext.list_shared_custom(cx, viewer_user_id, options).await {
        Ok(shared) => shared,
        Err(error) => {
            tracing::error!(%error, "pool extension listSharedCustom failed");
            Vec::new()
        }
    }
}

/// Whose eyes an upstream response's headers reach, decided per served candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderAudience {
    /// The caller's own row: headers pass as the delivery path already passes them.
    Own,
    /// A borrowed builtin row: the owner's account budget and identity are dropped
    /// ([`borrower_safe_headers`]).
    BorrowedBuiltin,
    /// A borrowed custom endpoint: an arbitrary gateway (LiteLLM, nginx, a CDN) whose headers
    /// may locate or identify it, so nothing passes but what reading the body needs.
    BorrowedCustom,
}

impl HeaderAudience {
    pub fn of(candidate: &RoutingCandidate) -> Self {
        match (&candidate.share, candidate.is_builtin) {
            (None, _) => Self::Own,
            (Some(_), true) => Self::BorrowedBuiltin,
            (Some(_), false) => Self::BorrowedCustom,
        }
    }

    /// The upstream headers this audience may see. For a borrowed custom endpoint that is
    /// `content-type` alone: the transport already decodes `content-encoding` (and drops the
    /// matching `content-length`), and every delivery path re-frames the body, so no other
    /// header is needed to read it.
    pub fn filter(self, headers: &HeaderMap) -> HeaderMap {
        match self {
            Self::Own => headers.clone(),
            Self::BorrowedBuiltin => borrower_safe_headers(headers),
            Self::BorrowedCustom => {
                let mut out = HeaderMap::new();
                if let Some(value) = headers.get(http::header::CONTENT_TYPE) {
                    out.insert(http::header::CONTENT_TYPE, value.clone());
                }
                out
            }
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

/// A fake edition lending custom endpoints, shared by the routing, catalog, route and dispatch
/// tests of shared custom endpoints.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use sqlx::PgPool;

    use super::*;
    use crate::db::custom_providers::{insert_custom_provider, NewCustomProvider};
    use crate::pool::StoredCredential;

    pub(crate) fn share_info() -> ShareInfo {
        ShareInfo { team_id: "team_1".into(), team_name: "Acme".into(), owner_label: "owner@example.com".into() }
    }

    /// `owner_id` stores one custom endpoint with its key (`sk-owner-<slug>`), returned the way
    /// an edition would lend it.
    pub(crate) async fn owner_endpoint(
        pool: &PgPool,
        owner_id: &str,
        slug: &str,
        format: &str,
        base_url: &str,
        manual_models: Option<&[&str]>,
    ) -> SharedCustomProvider {
        let manual_json = manual_models.map(|m| serde_json::json!(m).to_string());
        let provider = insert_custom_provider(
            pool,
            NewCustomProvider {
                user_id: owner_id,
                slug,
                name: &format!("{slug} endpoint"),
                format,
                base_url,
                count_tokens_url: None,
                models_mode: if manual_models.is_some() { "manual" } else { "auto" },
                manual_models_json: manual_json.as_deref(),
            },
        )
        .await
        .expect("insert owner endpoint")
        .expect("under the cap");
        let credential = StoredCredential { access_token: format!("sk-owner-{slug}"), ..Default::default() };
        let account = crate::db::test_support::insert_account(pool, owner_id, slug, &credential).await;
        SharedCustomProvider { provider, account, share: share_info(), usage: None }
    }

    /// Lends `shared` to every viewer (or fails when `fail`), and admits every attempt with a
    /// lease that records how it settled.
    #[derive(Default)]
    pub(crate) struct SharedCustomExtension {
        pub shared: Mutex<Vec<SharedCustomProvider>>,
        pub fail: bool,
        /// `options.usage` of every `list_shared_custom` call.
        pub list_calls: Mutex<Vec<bool>>,
        /// `(account id, share is set, custom row is set)` of every reserved candidate.
        pub reserved: Mutex<Vec<(String, bool, bool)>>,
        pub settled: Arc<Mutex<Vec<LeaseOutcome>>>,
    }

    impl SharedCustomExtension {
        pub(crate) fn lending(shared: Vec<SharedCustomProvider>) -> Arc<Self> {
            Arc::new(Self { shared: Mutex::new(shared), ..Default::default() })
        }
    }

    /// `test_state` with `ext` composed in as the edition's pool extension.
    pub(crate) fn state_with_extension(
        pool: PgPool,
        transport: Arc<crate::upstream::MockTransport>,
        ext: Arc<dyn PoolExtension>,
    ) -> AppState {
        AppState::builder(crate::db::test_support::test_config(), pool)
            .transport(transport)
            .pool_extension(Some(ext))
            .background_work(false)
            .build()
    }

    struct RecordingLease(Arc<Mutex<Vec<LeaseOutcome>>>);

    #[async_trait]
    impl AttemptLease for RecordingLease {
        async fn settle(&self, outcome: LeaseOutcome) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(outcome);
            Ok(())
        }
    }

    #[async_trait]
    impl PoolExtension for SharedCustomExtension {
        async fn list_shared(&self, _: &AppState, _: &str, _: ProviderId, _: ListSharedOptions) -> anyhow::Result<Vec<SharedAccount>> {
            Ok(Vec::new())
        }
        async fn list_shared_custom(&self, _: &AppState, _: &str, options: ListSharedOptions) -> anyhow::Result<Vec<SharedCustomProvider>> {
            self.list_calls.lock().unwrap().push(options.usage);
            if self.fail {
                anyhow::bail!("extension store unreachable");
            }
            let mut shared = self.shared.lock().unwrap().clone();
            if !options.usage {
                // Routing never asks for usage, so it never gets any.
                shared.iter_mut().for_each(|s| s.usage = None);
            }
            Ok(shared)
        }
        async fn reserve_attempt(&self, _: &AppState, _: ReserveContext<'_>, candidate: &RoutingCandidate) -> anyhow::Result<ReserveOutcome> {
            self.reserved.lock().unwrap().push((
                candidate.account.id.clone(),
                candidate.share.is_some(),
                candidate.custom_provider.is_some(),
            ));
            Ok(ReserveOutcome::Admitted(Box::new(RecordingLease(self.settled.clone()))))
        }
        async fn set_shared_priority(&self, _: &AppState, _: &str, _: &str, _: i32) -> anyhow::Result<bool> {
            Ok(false)
        }
    }
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

    #[test]
    fn a_borrowed_custom_endpoint_shows_nothing_but_the_content_type() {
        let mut h = HeaderMap::new();
        h.insert("content-type", "application/json".parse().unwrap());
        for name in ["x-litellm-model-api-base", "set-cookie", "server", "via", "alt-svc", "x-request-id", "cf-ray"] {
            h.insert(http::HeaderName::from_static(name), "owner-gateway.internal".parse().unwrap());
        }
        let custom = HeaderAudience::BorrowedCustom.filter(&h);
        assert_eq!(custom.len(), 1);
        assert_eq!(custom.get("content-type").unwrap(), "application/json");
        // Borrowed builtin rows keep the denylist; own rows pass everything.
        assert_eq!(HeaderAudience::BorrowedBuiltin.filter(&h), borrower_safe_headers(&h));
        assert_eq!(HeaderAudience::Own.filter(&h), h);
    }
}
