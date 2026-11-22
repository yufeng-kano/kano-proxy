//! What the server knows about an account's sign-in apart from its bench
//! (docs/providers.md § Sign-in state): the last failed OAuth refresh and the last upstream
//! `401`. Two columns, one writer each, so neither overwrites the other. Status surfaces and the
//! refresh retry pacing read them; routing does not.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::db::accounts::{iso_from_ms, parse_iso_ms, AccountRow};
use crate::pool::StoredCredential;

/// A failed refresh is not retried sooner than this unless the stored access token is known to
/// be past its expiry. Then every caller retries: the bench that follows the
/// upstream `401` is what paces those.
pub const REFRESH_RETRY_MS: i64 = 300_000;

/// The last failed OAuth token refresh, as stored in `refresh_failure_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefreshFailure {
    pub at: String,
    /// The token endpoint's HTTP status. `None` when it never answered, or answered `2xx`
    /// without a usable access token.
    pub status: Option<u16>,
    /// The token endpoint named the grant itself (`invalid_grant`): the refresh token is no
    /// longer accepted. A bare status does not say that. A `403` can be an edge refusing the
    /// call and a `400` a misconfigured client, and neither is fixed by signing in again.
    pub refused: bool,
    /// The stored access token's expiry when the refresh failed. `None` when the credential
    /// carries none.
    pub expires_at: Option<String>,
}

impl RefreshFailure {
    /// `expires_at` is re-serialized through the server's own ISO format so every stored value
    /// reads the same, whatever shape the credential's own field had.
    pub fn now(status: Option<u16>, refused: bool, credential: &StoredCredential, now_ms: i64) -> Self {
        let expires_at = credential.expires_at.as_deref().and_then(parse_iso_ms).map(iso_from_ms);
        Self { at: iso_from_ms(now_ms), status, refused, expires_at }
    }

    /// The stored fact says the access token has run out: the failure was never made good.
    pub fn token_ran_out(&self, now_ms: i64) -> bool {
        self.expires_ms().is_some_and(|expires| expires <= now_ms)
    }

    fn expires_ms(&self) -> Option<i64> {
        self.expires_at.as_deref().and_then(parse_iso_ms)
    }
}

pub fn read_refresh_failure(row: &AccountRow) -> Option<RefreshFailure> {
    serde_json::from_str(row.refresh_failure_json.as_deref()?).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    /// Nothing recorded against it.
    Ok,
    /// The refresh token was refused while the access token still works: requests keep being
    /// served until `until_ms`, and only signing in again keeps them going after it.
    Expiring { until_ms: i64 },
    /// Upstream refused the credential, or the access token ran out and could not be replaced.
    Expired,
}

/// The sign-in state the stored facts add up to at `now_ms`.
pub fn sign_in_state(row: &AccountRow, now_ms: i64) -> SignIn {
    if row.sign_in_rejected_at.is_some() {
        return SignIn::Expired;
    }
    let Some(failure) = read_refresh_failure(row) else { return SignIn::Ok };
    match (failure.refused, failure.expires_ms()) {
        (_, Some(expires)) if expires <= now_ms => SignIn::Expired,
        (true, Some(expires)) => SignIn::Expiring { until_ms: expires },
        // Refused with no expiry on record: there is no deadline to announce, and nothing says
        // the token has stopped working. The upstream `401` is what will.
        (true, None) | (false, _) => SignIn::Ok,
    }
}

/// How far ahead of a sign-in's stated end the Providers page starts saying so. Claude Code's
/// own client warns three days ahead at startup and ten in its non-interactive output; a pool
/// that goes quiet when its account does gets the week.
pub const SIGN_IN_NOTICE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The stated end of a sign-in (`refresh_expires_at`), when it is still ahead and inside the
/// notice window. Past it, the refresh that fails is what reports the sign-in expired.
pub fn sign_in_end_notice(credential: &StoredCredential, now_ms: i64) -> Option<i64> {
    let ends = credential.refresh_expires_at.as_deref().and_then(parse_iso_ms)?;
    (ends > now_ms && ends - now_ms <= SIGN_IN_NOTICE_MS).then_some(ends)
}

/// Whether a refresh that just failed should be left alone for now. Only a token known to have
/// run out is retried at once; one with time left, or with no expiry on record, may well still
/// work, so waiting costs nothing and asking on every request buys nothing.
pub fn refresh_retry_paused(row: &AccountRow, credential: &StoredCredential, now_ms: i64) -> bool {
    let Some(failure) = read_refresh_failure(row) else { return false };
    let Some(failed_at) = parse_iso_ms(&failure.at) else { return false };
    let token_ran_out = credential.expires_at.as_deref().and_then(parse_iso_ms).is_some_and(|exp| exp <= now_ms);
    !token_ran_out && now_ms - failed_at < REFRESH_RETRY_MS
}

/// Stores a failed refresh and releases the refresh lock in one compare-and-release statement.
/// A caller that no longer holds the lock writes nothing: a reconnect clears the lock, and its
/// fresh credential must not inherit the old one's failure.
pub async fn record_refresh_failure(
    db: &PgPool,
    account_id: &str,
    lock_token: &str,
    failure: &RefreshFailure,
) -> Result<bool, sqlx::Error> {
    let json = serde_json::to_string(failure).expect("refresh failure serializes");
    let r = sqlx::query(
        "UPDATE upstream_accounts SET refresh_failure_json = $1, refreshing_at = NULL
         WHERE id = $2 AND refreshing_at = $3",
    )
    .bind(json)
    .bind(account_id)
    .bind(lock_token)
    .execute(db)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Upstream answered `401` for this account's stored credential. Scoped to the row's owner,
/// like the bench written beside it.
pub async fn mark_sign_in_rejected(
    db: &PgPool,
    user_id: &str,
    provider: &str,
    account_id: &str,
    now_ms: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE upstream_accounts SET sign_in_rejected_at = $1
         WHERE id = $2 AND user_id = $3 AND provider = $4",
    )
    .bind(iso_from_ms(now_ms))
    .bind(account_id)
    .bind(user_id)
    .bind(provider)
    .execute(db)
    .await?;
    Ok(())
}

/// A request got through, so upstream accepts the credential: forget the refusal. The failed
/// refresh goes with it only when `and_refresh_failure`, which the caller sets when that record
/// says the token had run out. A refresh refused while the old token still works is not
/// disproved by the old token working, and keeps pacing the retry.
pub async fn clear_sign_in_failure(
    db: &PgPool,
    user_id: &str,
    account_id: &str,
    and_refresh_failure: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE upstream_accounts
         SET sign_in_rejected_at = NULL,
             refresh_failure_json = CASE WHEN $3 THEN NULL ELSE refresh_failure_json END
         WHERE id = $1 AND user_id = $2 AND (refresh_failure_json IS NOT NULL OR sign_in_rejected_at IS NOT NULL)",
    )
    .bind(account_id)
    .bind(user_id)
    .bind(and_refresh_failure)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::candidates::tests_support::account_row;

    const NOW: i64 = 1_800_000_000_000;

    fn credential(expires_in_ms: Option<i64>) -> StoredCredential {
        StoredCredential {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: expires_in_ms.map(|ms| iso_from_ms(NOW + ms)),
            ..Default::default()
        }
    }

    fn row_with(failure: Option<RefreshFailure>, rejected_at: Option<i64>) -> AccountRow {
        let mut row = account_row("a", "claude-code");
        row.refresh_failure_json = failure.map(|f| serde_json::to_string(&f).unwrap());
        row.sign_in_rejected_at = rejected_at.map(iso_from_ms);
        row
    }

    fn refused(credential: &StoredCredential) -> RefreshFailure {
        RefreshFailure::now(Some(400), true, credential, NOW)
    }

    #[test]
    fn a_refused_refresh_is_expiring_until_the_access_token_runs_out_and_expired_after() {
        let row = row_with(Some(refused(&credential(Some(600_000)))), None);
        assert_eq!(sign_in_state(&row, NOW), SignIn::Expiring { until_ms: NOW + 600_000 });
        assert_eq!(sign_in_state(&row, NOW + 599_999), SignIn::Expiring { until_ms: NOW + 600_000 });
        assert_eq!(sign_in_state(&row, NOW + 600_000), SignIn::Expired);
    }

    /// A status alone is not a refusal: an edge can answer 403 and a misconfigured client 400,
    /// and telling the operator to sign in again would fix neither.
    #[test]
    fn a_refresh_that_failed_without_naming_the_grant_says_nothing_until_the_token_is_dead() {
        for status in [None, Some(400), Some(401), Some(403), Some(429), Some(500), Some(503)] {
            let failure = RefreshFailure::now(status, false, &credential(Some(600_000)), NOW);
            let row = row_with(Some(failure), None);
            assert_eq!(sign_in_state(&row, NOW + 1), SignIn::Ok, "{status:?} before expiry");
            assert_eq!(sign_in_state(&row, NOW + 600_001), SignIn::Expired, "{status:?} after expiry");
        }
    }

    /// With no expiry on record there is no deadline to announce and no proof the token is
    /// dead, so the refusal changes nothing a status surface shows.
    #[test]
    fn a_failure_with_no_expiry_on_record_never_reads_expired_on_its_own() {
        assert_eq!(sign_in_state(&row_with(Some(refused(&credential(None))), None), NOW), SignIn::Ok);
        let failed = RefreshFailure::now(Some(502), false, &credential(None), NOW);
        assert_eq!(sign_in_state(&row_with(Some(failed), None), NOW + 86_400_000), SignIn::Ok);
    }

    #[test]
    fn an_upstream_401_is_expired_whatever_the_refresh_side_says() {
        assert_eq!(sign_in_state(&row_with(None, Some(NOW - 9_000_000)), NOW), SignIn::Expired);
        let row = row_with(Some(refused(&credential(Some(600_000)))), Some(NOW));
        assert_eq!(sign_in_state(&row, NOW), SignIn::Expired);
        assert_eq!(sign_in_state(&row_with(None, None), NOW), SignIn::Ok);
    }

    #[test]
    fn the_expiry_is_stored_in_one_format_whatever_the_credential_used() {
        let offset = StoredCredential { expires_at: Some("2027-01-15T08:00:00+00:00".into()), ..credential(None) };
        assert_eq!(refused(&offset).expires_at.as_deref(), Some("2027-01-15T08:00:00.000Z"));
        let junk = StoredCredential { expires_at: Some("soon".into()), ..credential(None) };
        assert_eq!(refused(&junk).expires_at, None);
    }

    #[test]
    fn a_failed_refresh_is_retried_at_once_only_when_the_token_is_known_to_have_run_out() {
        let live = credential(Some(1_200_000));
        let row = row_with(Some(RefreshFailure::now(Some(503), false, &live, NOW)), None);
        assert!(refresh_retry_paused(&row, &live, NOW + 1));
        assert!(refresh_retry_paused(&row, &live, NOW + REFRESH_RETRY_MS - 1));
        assert!(!refresh_retry_paused(&row, &live, NOW + REFRESH_RETRY_MS), "the interval is over");
        // A dead token cannot be made worse by retrying, and a retry is its only way back.
        assert!(!refresh_retry_paused(&row, &credential(Some(-1)), NOW + 1));
        // One with no expiry on record may well still work: asking on every request buys nothing.
        assert!(refresh_retry_paused(&row, &credential(None), NOW + 1));
        assert!(!refresh_retry_paused(&row_with(None, None), &live, NOW + 1), "nothing failed");
    }

    #[test]
    fn a_stated_end_is_noticed_only_inside_the_week_before_it() {
        let ending_in = |ms: i64| StoredCredential { refresh_expires_at: Some(iso_from_ms(NOW + ms)), ..credential(Some(600_000)) };
        assert_eq!(sign_in_end_notice(&ending_in(3 * 86_400_000), NOW), Some(NOW + 3 * 86_400_000));
        assert_eq!(sign_in_end_notice(&ending_in(SIGN_IN_NOTICE_MS), NOW), Some(NOW + SIGN_IN_NOTICE_MS));
        assert_eq!(sign_in_end_notice(&ending_in(SIGN_IN_NOTICE_MS + 1), NOW), None, "too far off to act on");
        // Past it the failed refresh is what speaks, not a date in the past.
        assert_eq!(sign_in_end_notice(&ending_in(0), NOW), None);
        assert_eq!(sign_in_end_notice(&credential(Some(600_000)), NOW), None, "a provider that states none");
    }
}
