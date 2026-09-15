//! OAuth refresh single-flight for every adapter entry point (apps/api/src/providers/refresh.ts).
//! The winner reloads after claiming the row lock, persists its new encrypted payload while
//! compare-releasing; losers briefly poll for that persisted credential before fail-opening
//! to their old one.

use std::future::Future;
use std::time::Duration;

use crate::crypto::token_crypto::{decrypt_json, encrypt_json};
use crate::db::accounts::{acquire_refresh_lock, get_account, release_refresh_lock, write_refreshed_credential};
use crate::pool::{AcquiredAccount, StoredCredential};
use crate::AppState;

const REFRESH_POLL_ATTEMPTS: u32 = 3;
const REFRESH_POLL_DELAY: Duration = Duration::from_millis(25);

pub async fn refresh_oauth_credential<N, R, Fut>(
    cx: &AppState,
    account: AcquiredAccount,
    needs_refresh: N,
    refresh: R,
) -> AcquiredAccount
where
    N: Fn(&StoredCredential) -> bool,
    R: Fn(&StoredCredential) -> Fut,
    Fut: Future<Output = Option<StoredCredential>>,
{
    if !needs_refresh(&account.credential) {
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
            if !needs_refresh(&credential) {
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
                if !needs_refresh(&credential) {
                    let _ = release_refresh_lock(db, &row.id, &lock_token).await;
                    return Ok(AcquiredAccount { row, credential });
                }
                let Some(refreshed) = refresh(&credential).await else {
                    let _ = release_refresh_lock(db, &row.id, &lock_token).await;
                    return Ok(AcquiredAccount { row, credential });
                };
                let encrypted = encrypt_json(key, &refreshed).map_err(|_| ())?;
                let persisted = write_refreshed_credential(db, &row.id, &lock_token, &encrypted).await.map_err(|_| ())?;
                if persisted {
                    let mut row = row;
                    row.encrypted_payload = encrypted;
                    row.refreshing_at = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_account, insert_user, skip_without_db, test_pool, test_state};
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
            |_| async { Some(StoredCredential { access_token: "new".into(), refresh_token: Some("r2".into()), ..Default::default() }) },
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
        let out = refresh_oauth_credential(&cx, account, |_| true, |_| async { None }).await;
        assert_eq!(out.credential.access_token, "old");
        let fresh = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert!(fresh.refreshing_at.is_none());
    }
}
