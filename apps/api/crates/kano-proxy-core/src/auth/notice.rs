//! The usage notice every signed-in user accepts before any session route answers
//! (docs/auth.md § Usage notice). The text lives in the web catalog; the server knows only
//! its version and who accepted which.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use time::macros::format_description;

use crate::db::users::UserRow;

/// Date of the notice's current text. Change it in the same commit that changes the text in
/// `apps/web/src/i18n/en.ts` (`notice.*`): a new date asks every user to accept again.
pub const USAGE_NOTICE_DATE: &str = "2026-11-02";

/// The notice a user is asked to accept: the core notice, plus the edition's own terms when it
/// has any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageNotice {
    /// What `users.notice_version` must equal: `<notice date>` or `<notice date>+<terms date>`.
    pub version: String,
    /// The later of the two dates, which the page prints as "Notice updated".
    pub updated: &'static str,
}

impl UsageNotice {
    /// Panics on a terms date that is not `YYYY-MM-DD`, so a typo stops the server at start
    /// instead of silently asking nobody, or everybody, again.
    pub fn new(terms_date: Option<&'static str>) -> Self {
        let Some(terms_date) = terms_date else {
            return Self { version: USAGE_NOTICE_DATE.to_string(), updated: USAGE_NOTICE_DATE };
        };
        assert!(
            time::Date::parse(terms_date, format_description!("[year]-[month]-[day]")).is_ok(),
            "terms_date must be YYYY-MM-DD, got {terms_date:?}"
        );
        // ISO dates order as strings.
        Self { version: format!("{USAGE_NOTICE_DATE}+{terms_date}"), updated: USAGE_NOTICE_DATE.max(terms_date) }
    }

    pub fn accepted_by(&self, user: &UserRow) -> bool {
        user.notice_version.as_deref() == Some(self.version.as_str())
    }

    /// The `notice` object of `GET /api/auth/me` and `POST /api/auth/notice`.
    pub fn status(&self, user: &UserRow) -> Value {
        json!({ "version": self.version, "updated": self.updated, "accepted": self.accepted_by(user) })
    }
}

/// What a session route answers a user who has not accepted the current notice. The SPA keys on
/// the exact body to send the user to the notice page.
pub fn notice_required_response() -> Response {
    (StatusCode::FORBIDDEN, Json(json!({ "error": "notice_required" }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_core_notice_alone_is_its_own_date() {
        let notice = UsageNotice::new(None);
        assert_eq!(notice.version, USAGE_NOTICE_DATE);
        assert_eq!(notice.updated, USAGE_NOTICE_DATE);
    }

    #[test]
    fn edition_terms_join_the_version_and_the_later_date_is_shown() {
        let later = UsageNotice::new(Some("2099-01-31"));
        assert_eq!(later.version, format!("{USAGE_NOTICE_DATE}+2099-01-31"));
        assert_eq!(later.updated, "2099-01-31");
        // Terms older than the notice still change the version, but the shown date is the notice's.
        let earlier = UsageNotice::new(Some("2000-01-01"));
        assert_eq!(earlier.version, format!("{USAGE_NOTICE_DATE}+2000-01-01"));
        assert_eq!(earlier.updated, USAGE_NOTICE_DATE);
    }

    #[test]
    #[should_panic(expected = "terms_date must be YYYY-MM-DD")]
    fn a_malformed_terms_date_stops_the_server() {
        UsageNotice::new(Some("2026-13-01"));
    }

    #[test]
    fn only_the_exact_version_counts_as_accepted() {
        let notice = UsageNotice::new(Some("2099-01-31"));
        let mut user = UserRow {
            id: "usr_1".into(),
            google_sub: "sub".into(),
            email: "a@example.com".into(),
            name: None,
            picture_url: None,
            created_at: "t".into(),
            updated_at: "t".into(),
            notice_version: None,
            notice_accepted_at: None,
        };
        assert!(!notice.accepted_by(&user), "never accepted");
        user.notice_version = Some(USAGE_NOTICE_DATE.into());
        assert!(!notice.accepted_by(&user), "the core notice alone is not the edition's version");
        user.notice_version = Some(notice.version.clone());
        assert!(notice.accepted_by(&user));
        assert_eq!(notice.status(&user), json!({ "version": notice.version, "updated": "2099-01-31", "accepted": true }));
    }
}
