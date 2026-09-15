//! Session cookie format (apps/api/src/auth/session.ts): `kano-proxy_session=<id>.<hex
//! HMAC-SHA256(SESSION_SECRET, id)>; Path=/; HttpOnly; SameSite=Lax; Max-Age=1209600`
//! plus `Secure` on HTTPS. The database row carries the 14-day expiry.

use super::{hmac_sha256_hex, timing_safe_equal};

pub const COOKIE_NAME: &str = "kano-proxy_session";
pub const SESSION_DAYS: i64 = 14;

pub fn sign_session_id(secret: &str, session_id: &str) -> String {
    hmac_sha256_hex(secret, session_id.as_bytes())
}

pub fn session_cookie(secret: &str, session_id: &str, secure: bool) -> String {
    let sig = sign_session_id(secret, session_id);
    format!(
        "{COOKIE_NAME}={session_id}.{sig}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        SESSION_DAYS * 86_400,
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_session_cookie(secure: bool) -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}", if secure { "; Secure" } else { "" })
}

/// Raw cookie value (`id.sig`) from a Cookie header, unverified.
pub fn cookie_value(cookie_header: &str) -> Option<&str> {
    cookie_header.split(';').map(str::trim).find_map(|part| part.strip_prefix(&format!("{COOKIE_NAME}=")))
}

/// The session id from the Cookie header when its signature verifies.
pub fn verified_session_id(secret: &str, cookie_header: &str) -> Option<String> {
    let value = cookie_value(cookie_header)?;
    let (id, sig) = value.split_once('.')?;
    if id.is_empty() || sig.is_empty() {
        return None;
    }
    timing_safe_equal(&sign_session_id(secret, id), sig).then(|| id.to_string())
}

/// The unverified session id, for logout (mirrors `getCookieSessionId`).
pub fn unverified_session_id(cookie_header: &str) -> Option<&str> {
    cookie_value(cookie_header)?.split('.').next().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_webcrypto_hmac() {
        // node: crypto.createHmac('sha256','secret').update('sess_1').digest('hex')
        assert_eq!(sign_session_id("secret", "sess_1"), "a1a59470163561725573b352e24b1bb4babda6832420c97ca533cab2584f3606");
        // RFC 4231 test case 2 pins the HMAC primitive itself.
        assert_eq!(
            hmac_sha256_hex("Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn cookie_round_trip() {
        let cookie = session_cookie("s", "sess_abc", true);
        assert!(cookie.starts_with("kano-proxy_session=sess_abc."));
        assert!(cookie.ends_with("; Path=/; HttpOnly; SameSite=Lax; Max-Age=1209600; Secure"));
        let header = format!("other=1; {}", cookie.split(';').next().unwrap());
        assert_eq!(verified_session_id("s", &header).as_deref(), Some("sess_abc"));
        assert_eq!(verified_session_id("wrong", &header), None);
        assert_eq!(unverified_session_id(&header), Some("sess_abc"));
        assert_eq!(verified_session_id("s", "kano-proxy_session=sess_abc"), None);
    }
}
