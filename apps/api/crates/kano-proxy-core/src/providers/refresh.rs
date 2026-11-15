//! OAuth refresh single-flight for every adapter entry point.
//! The winner reloads after claiming the row lock, persists its new encrypted payload while
//! compare-releasing; losers briefly poll for that persisted credential before fail-opening
//! to their old one. A refresh that fails is recorded on the row (`pool::sign_in`): the record
//! paces the retry while the old access token still works, and is what the Providers page reads
//! to say a sign-in is about to stop working.

use std::future::Future;
use std::time::Duration;

use crate::crypto::token_crypto::{decrypt_json, encrypt_json};
use crate::db::accounts::{acquire_refresh_lock, get_account, release_refresh_lock, write_refreshed_credential};
use crate::pool::sign_in::{record_refresh_failure, refresh_retry_paused, RefreshFailure};
use crate::pool::{AcquiredAccount, StoredCredential};
use crate::AppState;

/// How long a caller that lost the refresh race waits for the winner's credential before it
/// goes ahead with its own: three seconds, a token endpoint round trip with room to spare. The
/// wait used to be 75ms, shorter than that round trip, so the losers of nearly every refresh
/// went out with the token the winner was about to make worthless (docs/providers.md § OAuth
/// refresh single-flight).
const REFRESH_POLL_ATTEMPTS: u32 = 60;
const REFRESH_POLL_DELAY: Duration = Duration::from_millis(50);

/// Why an adapter's refresh call produced no credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshError {
    /// Nothing was sent: the credential or the deploy lacks what the grant needs. Not recorded.
    NotAttempted,
    /// The token endpoint answered with this non-2xx status. `refused` when the answer named the
    /// grant itself, which is the one failure signing in again fixes.
    Status { status: u16, refused: bool },
    /// No answer, or a `2xx` without a usable access token.
    Unusable,
}

impl RefreshError {
    /// Reads a token endpoint's non-2xx answer. Only the OAuth error code is looked at
    /// (`{"error": "invalid_grant"}`, RFC 6749 § 5.2); nothing else of the body is kept.
    pub async fn from_response(response: crate::upstream::UpstreamResponse) -> Self {
        let status = response.status.as_u16();
        let refused = response
            .json_value()
            .await
            .ok()
            .is_some_and(|body| body.get("error").and_then(serde_json::Value::as_str) == Some("invalid_grant"));
        RefreshError::Status { status, refused }
    }

    fn status(self) -> Option<u16> {
        match self {
            RefreshError::Status { status, .. } => Some(status),
            RefreshError::NotAttempted | RefreshError::Unusable => None,
        }
    }

    fn refused(self) -> bool {
        matches!(self, RefreshError::Status { refused: true, .. })
    }
}

pub async fn refresh_oauth_credential<N, R, Fut>(
    cx: &AppState,
    account: AcquiredAccount,
    needs_refresh: N,
    refresh: R,
) -> AcquiredAccount
where
    N: Fn(&StoredCredential) -> bool,
    R: Fn(&StoredCredential) -> Fut,
    Fut: Future<Output = Result<StoredCredential, RefreshError>>,
{
    // Due, and not a retry of a refresh that just failed while the old token still works.
    let due = |row: &crate::db::AccountRow, credential: &StoredCredential| {
        needs_refresh(credential) && !refresh_retry_paused(row, credential, crate::app::now_ms())
    };
    if !due(&account.row, &account.credential) {
        return account;
    }
    let key = cx.config().token_encryption_key.as_deref();
    let db = cx.pool();
    for attempt in 0..=REFRESH_POLL_ATTEMPTS {
        if attempt > 0 {
            let row = match get_account(db, &account.row.user_id, &account.row.id).await {
                Ok(Some(row)) => row,
                _ => return account,
            };
            let credential: StoredCredential = match decrypt_json(key, &row.encrypted_payload) {
                Ok(c) => c,
                Err(_) => return account,
            };
            if !due(&row, &credential) {
                return AcquiredAccount { row, credential };
            }
            if row.refreshing_at.is_some() {
                if attempt < REFRESH_POLL_ATTEMPTS {
                    tokio::time::sleep(REFRESH_POLL_DELAY).await;
                }
                continue;
            }
        }

        let lock_token = match acquire_refresh_lock(db, &account.row.id).await {
            Ok(t) => t,
            Err(_) => return account,
        };
        if let Some(lock_token) = lock_token {
            let outcome: Result<AcquiredAccount, ()> = async {
                let row = match get_account(db, &account.row.user_id, &account.row.id).await {
                    Ok(Some(row)) => row,
                    _ => {
                        let _ = release_refresh_lock(db, &account.row.id, &lock_token).await;
                        return Ok(account.clone());
                    }
                };
                let credential: StoredCredential = decrypt_json(key, &row.encrypted_payload).map_err(|_| ())?;
                if !due(&row, &credential) {
                    let _ = release_refresh_lock(db, &row.id, &lock_token).await;
                    return Ok(AcquiredAccount { row, credential });
                }
                let refreshed = match refresh(&credential).await {
                    Ok(refreshed) => refreshed,
                    Err(RefreshError::NotAttempted) => {
                        let _ = release_refresh_lock(db, &row.id, &lock_token).await;
                        return Ok(AcquiredAccount { row, credential });
                    }
                    Err(error) => return Ok(keep_old_credential(cx, row, credential, &lock_token, error).await),
                };
                let encrypted = encrypt_json(key, &refreshed).map_err(|_| ())?;
                let persisted = write_refreshed_credential(db, &row.id, &lock_token, &encrypted).await.map_err(|_| ())?;
                if persisted {
                    let mut row = row;
                    row.encrypted_payload = encrypted;
                    row.refreshing_at = None;
                    row.refresh_failure_json = None;
                    row.sign_in_rejected_at = None;
                    Ok(AcquiredAccount { row, credential: refreshed })
                } else {
                    Ok(account.clone())
                }
            }
            .await;
            return match outcome {
                Ok(acquired) => acquired,
                Err(()) => {
                    let _ = release_refresh_lock(db, &account.row.id, &lock_token).await;
                    account
                }
            };
        }

        if attempt < REFRESH_POLL_ATTEMPTS {
            tokio::time::sleep(REFRESH_POLL_DELAY).await;
        }
    }
    // Lock stayed held and its credential remained expired: preserve availability.
    account
}

/// A refresh the token endpoint did not honour: record it and release the lock in one
/// statement, then carry on with the stored credential. Only the status is logged or stored,
/// never the answer's body.
async fn keep_old_credential(
    cx: &AppState,
    mut row: crate::db::AccountRow,
    credential: StoredCredential,
    lock_token: &str,
    error: RefreshError,
) -> AcquiredAccount {
    let failure = RefreshFailure::now(error.status(), error.refused(), &credential, crate::app::now_ms());
    tracing::warn!(
        account_id = %row.id,
        provider = %row.provider,
        status = ?failure.status,
        refused = failure.refused,
        token_expires_at = ?failure.expires_at,
        "OAuth refresh failed; keeping the stored credential"
    );
    match record_refresh_failure(cx.pool(), &row.id, lock_token, &failure).await {
        Ok(true) => {
            row.refresh_failure_json = serde_json::to_string(&failure).ok();
            row.refreshing_at = None;
        }
        // The lock was taken from under this caller (a reconnect, or the 30s breaker): whoever
        // holds the row now owns its state.
        Ok(false) => {}
        Err(_) => {
            let _ = release_refresh_lock(cx.pool(), &row.id, lock_token).await;
        }
    }
    AcquiredAccount { row, credential }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::accounts::{iso_from_ms, parse_iso_ms};
    use crate::db::test_support::{insert_account, insert_user, skip_without_db, test_pool, test_state};
    use crate::pool::sign_in::{read_refresh_failure, sign_in_state, SignIn, REFRESH_RETRY_MS};
    use crate::upstream::MockTransport;

    #[tokio::test]
    async fn refreshes_persists_and_serves_fresh_credential_to_losers() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let user = insert_user(&pool, "refresh@example.com").await;
        let stale = StoredCredential { access_token: "old".into(), refresh_token: Some("r".into()), ..Default::default() };
        let row = insert_account(&pool, &user.id, "claude-code", &stale).await;
        let account = AcquiredAccount { row, credential: stale };
        let refreshed = refresh_oauth_credential(
            &cx,
            account.clone(),
            |c| c.access_token == "old",
            |_| async { Ok(StoredCredential { access_token: "new".into(), refresh_token: Some("r2".into()), ..Default::default() }) },
        )
        .await;
        assert_eq!(refreshed.credential.access_token, "new");
        assert!(refreshed.row.refreshing_at.is_none());
        // A second caller holding the stale credential sees the persisted one without refreshing.
        let again = refresh_oauth_credential(&cx, account, |c| c.access_token == "old", |_| async { panic!("must not refresh") }).await;
        assert_eq!(again.credential.access_token, "new");
    }

    #[tokio::test]
    async fn failed_refresh_releases_lock_and_keeps_old_credential() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let user = insert_user(&pool, "refresh2@example.com").await;
        let stale = StoredCredential { access_token: "old".into(), ..Default::default() };
        let row = insert_account(&pool, &user.id, "codex", &stale).await;
        let account = AcquiredAccount { row: row.clone(), credential: stale };
        let out = refresh_oauth_credential(&cx, account, |_| true, |_| async { Err(RefreshError::Status { status: 503, refused: false }) }).await;
        assert_eq!(out.credential.access_token, "old");
        let fresh = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert!(fresh.refreshing_at.is_none());
        assert_eq!(read_refresh_failure(&fresh).and_then(|f| f.status), Some(503));
        assert_eq!(out.row.refresh_failure_json, fresh.refresh_failure_json, "the caller holds the row as stored");
    }

    fn expiring_in(ms: i64) -> StoredCredential {
        StoredCredential {
            access_token: "old".into(),
            refresh_token: Some("r".into()),
            expires_at: Some(iso_from_ms(crate::app::now_ms() + ms)),
            ..Default::default()
        }
    }

    /// The early-refresh case: the token endpoint refuses while the old access token has ten
    /// minutes left. The refusal is stored with that deadline, the next caller does not ask
    /// again inside the retry interval, and a later success wipes the record.
    #[tokio::test]
    async fn a_refused_refresh_is_recorded_then_paced_then_forgotten_on_success() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let user = insert_user(&pool, "refresh3@example.com").await;
        let live = expiring_in(600_000);
        let row = insert_account(&pool, &user.id, "claude-code", &live).await;
        let account = AcquiredAccount { row: row.clone(), credential: live.clone() };

        let out = refresh_oauth_credential(&cx, account.clone(), |_| true, |_| async { Err(RefreshError::Status { status: 400, refused: true }) }).await;
        assert_eq!(out.credential.access_token, "old");
        let stored = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert!(stored.refreshing_at.is_none());
        let failure = read_refresh_failure(&stored).expect("recorded");
        assert_eq!((failure.status, failure.expires_at.as_deref()), (Some(400), live.expires_at.as_deref()));
        let until_ms = parse_iso_ms(live.expires_at.as_deref().unwrap()).unwrap();
        assert_eq!(sign_in_state(&stored, crate::app::now_ms()), SignIn::Expiring { until_ms });

        // A concurrent request still holding the row from before the failure.
        let again = refresh_oauth_credential(&cx, account.clone(), |_| true, |_| async { panic!("asked again inside the retry interval") }).await;
        assert_eq!(again.credential.access_token, "old");
        assert!(get_account(&pool, &user.id, &row.id).await.unwrap().unwrap().refreshing_at.is_none());

        // The interval is over, and upstream had refused the old token in the meantime.
        let earlier = RefreshFailure { at: iso_from_ms(crate::app::now_ms() - REFRESH_RETRY_MS - 1), ..failure };
        sqlx::query("UPDATE upstream_accounts SET refresh_failure_json = $1, sign_in_rejected_at = $2 WHERE id = $3")
            .bind(serde_json::to_string(&earlier).unwrap())
            .bind(iso_from_ms(crate::app::now_ms()))
            .bind(&row.id)
            .execute(&pool)
            .await
            .unwrap();
        let renewed = refresh_oauth_credential(&cx, account, |c| c.access_token == "old", |_| async {
            Ok(StoredCredential { access_token: "new".into(), refresh_token: Some("r2".into()), ..Default::default() })
        })
        .await;
        assert_eq!(renewed.credential.access_token, "new");
        let stored = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert!(stored.refresh_failure_json.is_none() && stored.sign_in_rejected_at.is_none());
        assert_eq!(sign_in_state(&renewed.row, crate::app::now_ms()), SignIn::Ok);
    }

    /// Past its expiry the old token is no use to anyone, so every caller asks again, as before.
    #[tokio::test]
    async fn a_dead_token_is_never_paced() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let user = insert_user(&pool, "refresh4@example.com").await;
        let dead = expiring_in(-1_000);
        let row = insert_account(&pool, &user.id, "claude-code", &dead).await;
        let account = AcquiredAccount { row: row.clone(), credential: dead };
        let asked = std::sync::atomic::AtomicUsize::new(0);
        for _ in 0..2 {
            refresh_oauth_credential(&cx, account.clone(), |_| true, |_| {
                asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Err(RefreshError::Unusable) }
            })
            .await;
        }
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 2);
        let stored = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(read_refresh_failure(&stored).map(|f| f.status), Some(None));
        assert_eq!(sign_in_state(&stored, crate::app::now_ms()), SignIn::Expired);
    }

    /// A grant that was never sent is not a failed one, and a reconnect that lands while the
    /// refresh is in flight takes the lock with it: the new credential inherits nothing.
    #[tokio::test]
    async fn nothing_is_recorded_for_an_unsent_refresh_or_once_the_lock_is_gone() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let user = insert_user(&pool, "refresh5@example.com").await;
        let live = expiring_in(600_000);
        let row = insert_account(&pool, &user.id, "antigravity", &live).await;
        let account = AcquiredAccount { row: row.clone(), credential: live };

        refresh_oauth_credential(&cx, account.clone(), |_| true, |_| async { Err(RefreshError::NotAttempted) }).await;
        let stored = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert!(stored.refresh_failure_json.is_none() && stored.refreshing_at.is_none());

        let out = refresh_oauth_credential(&cx, account, |_| true, |_| {
            let pool = pool.clone();
            let id = row.id.clone();
            async move {
                sqlx::query("UPDATE upstream_accounts SET refreshing_at = NULL WHERE id = $1").bind(id).execute(&pool).await.unwrap();
                Err(RefreshError::Status { status: 400, refused: true })
            }
        })
        .await;
        assert!(out.row.refresh_failure_json.is_none());
        assert!(get_account(&pool, &user.id, &row.id).await.unwrap().unwrap().refresh_failure_json.is_none());
    }

    /// The loser of a refresh race waits for the winner's credential. A token endpoint round
    /// trip takes a few hundred milliseconds; the old 75ms wait gave up before it and sent the
    /// token the winner was about to make worthless.
    #[tokio::test]
    async fn a_caller_that_lost_the_race_waits_for_the_winners_credential() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let user = insert_user(&pool, "refresh6@example.com").await;
        let stale = StoredCredential { access_token: "old".into(), refresh_token: Some("r".into()), ..Default::default() };
        let row = insert_account(&pool, &user.id, "claude-code", &stale).await;
        let lock = acquire_refresh_lock(&pool, &row.id).await.unwrap().expect("the winner holds the lock");
        let renewed = StoredCredential { access_token: "new".into(), ..stale.clone() };
        let blob = encrypt_json(cx.config().token_encryption_key.as_deref(), &renewed).unwrap();
        let (winner_pool, id) = (pool.clone(), row.id.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            write_refreshed_credential(&winner_pool, &id, &lock, &blob).await.unwrap();
        });

        let account = AcquiredAccount { row, credential: stale };
        let out = refresh_oauth_credential(&cx, account, |c| c.access_token == "old", |_| async { panic!("the loser must not refresh") }).await;
        assert_eq!(out.credential.access_token, "new");
    }

    /// A refresh that succeeds ends the bench an upstream 401 earned the token it replaced, and
    /// only that one: a rate-limit bench is about the account and outlives the token.
    #[tokio::test]
    async fn a_successful_refresh_ends_a_401_bench_and_no_other() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let user = insert_user(&pool, "refresh7@example.com").await;
        let now = crate::app::now_ms();
        for (reason, still_benched) in [("401", false), ("429", true)] {
            let dead = expiring_in(-1_000);
            let row = insert_account(&pool, &user.id, "claude-code", &dead).await;
            crate::pool::bench::mark_benched(&pool, &user.id, "claude-code", &row.id, Some(300_000), Some(reason), now).await.unwrap();
            let account = AcquiredAccount { row: row.clone(), credential: dead };
            let renewed = refresh_oauth_credential(&cx, account, |c| c.access_token == "old", |_| async {
                Ok(StoredCredential { access_token: "new".into(), refresh_token: Some("r2".into()), ..Default::default() })
            })
            .await;
            assert_eq!(renewed.credential.access_token, "new");
            assert_eq!(crate::pool::bench::is_benched(&pool, &user.id, &row.id, now).await.unwrap(), still_benched, "{reason}");
        }
    }

    #[tokio::test]
    async fn only_an_answer_naming_the_grant_counts_as_a_refusal() {
        use crate::upstream::UpstreamResponse;
        use http::StatusCode;
        use serde_json::json;
        let read = |status: StatusCode, body: serde_json::Value| async move {
            RefreshError::from_response(UpstreamResponse::json(status, &body)).await
        };
        assert_eq!(
            read(StatusCode::BAD_REQUEST, json!({ "error": "invalid_grant", "error_description": "Refresh token not found or invalid" })).await,
            RefreshError::Status { status: 400, refused: true }
        );
        // The same status for a reason signing in again does not fix.
        assert_eq!(read(StatusCode::BAD_REQUEST, json!({ "error": "invalid_client" })).await, RefreshError::Status { status: 400, refused: false });
        assert_eq!(read(StatusCode::FORBIDDEN, json!({ "error": { "message": "blocked" } })).await, RefreshError::Status { status: 403, refused: false });
        assert_eq!(read(StatusCode::TOO_MANY_REQUESTS, json!({})).await, RefreshError::Status { status: 429, refused: false });
    }
}
