//! Resolve email / display name after OAuth for account pool labels
//! (apps/api/src/providers/identity.ts). Every call goes through the transport and fails
//! open to `None`s.

use base64::Engine;
use serde_json::Value;

use crate::upstream::UpstreamRequest;
use crate::AppState;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub email: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexIdentity {
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub plan: Option<String>,
}

fn non_empty(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

async fn get_json(cx: &AppState, req: UpstreamRequest) -> Option<Value> {
    let res = cx.transport().send(req).await.ok()?;
    if !res.status.is_success() {
        return None;
    }
    res.json_value().await.ok()
}

pub async fn fetch_claude_identity(cx: &AppState, access_token: &str) -> Identity {
    let req = UpstreamRequest::get("https://api.anthropic.com/api/oauth/profile")
        .header("authorization", &format!("Bearer {access_token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01");
    let Some(json) = get_json(cx, req).await else { return Identity::default() };
    let account = json.get("account");
    let display_name = non_empty(account.and_then(|a| a.get("display_name")));
    let email = display_name.clone().or_else(|| non_empty(account.and_then(|a| a.get("email"))));
    Identity { email, display_name }
}

/// `usage_json` is the codex usage fetch (providers/codex_usage.rs) the adapter supplies, so
/// this module stays free of provider-specific calls: `Some(payload)` when the usage
/// endpoint answered, with `email` and `plan_type` fields.
pub fn codex_identity(access_token: &str, account_id: Option<&str>, usage_payload: Option<&Value>) -> CodexIdentity {
    let from_jwt = email_from_jwt(access_token);
    if account_id.is_none() {
        return CodexIdentity { email: from_jwt, display_name: None, plan: None };
    }
    match usage_payload {
        Some(payload) => CodexIdentity {
            email: non_empty(payload.get("email")).or(from_jwt),
            display_name: None,
            plan: non_empty(payload.get("plan_type")),
        },
        None => CodexIdentity { email: from_jwt, display_name: None, plan: None },
    }
}

pub async fn fetch_grok_identity(cx: &AppState, access_token: &str) -> Identity {
    let req = UpstreamRequest::get("https://cli-chat-proxy.grok.com/v1/user")
        .header("authorization", &format!("Bearer {access_token}"))
        .header("accept", "application/json");
    let Some(json) = get_json(cx, req).await else { return Identity::default() };
    let display_name = non_empty(json.get("name"))
        .or_else(|| non_empty(json.get("username")))
        .or_else(|| non_empty(json.get("preferred_username")));
    let email = display_name.clone().or_else(|| non_empty(json.get("email")));
    Identity { email, display_name }
}

/// Google userinfo — the only identity Antigravity's scopes expose.
pub async fn fetch_antigravity_identity(cx: &AppState, access_token: &str) -> Identity {
    let req = UpstreamRequest::get("https://www.googleapis.com/oauth2/v2/userinfo?alt=json")
        .header("authorization", &format!("Bearer {access_token}"))
        .header("accept", "application/json");
    let Some(json) = get_json(cx, req).await else { return Identity::default() };
    Identity { email: non_empty(json.get("email")), display_name: non_empty(json.get("name")) }
}

pub fn email_from_jwt(access_token: &str) -> Option<String> {
    let mid = access_token.split('.').nth(1)?;
    if mid.is_empty() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(mid.trim_end_matches('=')).ok()?;
    let json: Value = serde_json::from_slice(&bytes).ok()?;
    let direct = non_empty(json.get("email"))
        .or_else(|| non_empty(json.get("preferred_username")))
        .or_else(|| non_empty(json.get("name")));
    if direct.is_some() {
        return direct;
    }
    non_empty(json.get("https://api.openai.com/profile").and_then(|p| p.get("email")))
        .or_else(|| non_empty(json.get("https://api.openai.com/auth").and_then(|p| p.get("email"))))
}

pub fn pick_account_label(email: Option<&str>, display_name: Option<&str>, fallback: &str) -> String {
    email
        .filter(|s| !s.is_empty())
        .or(display_name.filter(|s| !s.is_empty()))
        .unwrap_or(fallback)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: &str) -> String {
        format!("h.{}.s", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload))
    }

    #[test]
    fn email_from_jwt_prefers_direct_then_openai_claims() {
        assert_eq!(email_from_jwt(&jwt(r#"{"email":"a@b.c"}"#)).as_deref(), Some("a@b.c"));
        assert_eq!(email_from_jwt(&jwt(r#"{"https://api.openai.com/profile":{"email":"p@b.c"}}"#)).as_deref(), Some("p@b.c"));
        assert_eq!(email_from_jwt(&jwt(r#"{"https://api.openai.com/auth":{"email":"n@b.c"}}"#)).as_deref(), Some("n@b.c"));
        assert_eq!(email_from_jwt("nodots"), None);
        assert_eq!(email_from_jwt(&jwt("not json")), None);
    }

    #[test]
    fn codex_identity_without_account_id_uses_jwt_only() {
        let id = codex_identity(&jwt(r#"{"email":"j@b.c"}"#), None, Some(&serde_json::json!({"email":"u@b.c","plan_type":"plus"})));
        assert_eq!(id, CodexIdentity { email: Some("j@b.c".into()), display_name: None, plan: None });
        let id = codex_identity(&jwt(r#"{"email":"j@b.c"}"#), Some("acc"), Some(&serde_json::json!({"plan_type":"plus"})));
        assert_eq!(id.email.as_deref(), Some("j@b.c"));
        assert_eq!(id.plan.as_deref(), Some("plus"));
    }

    #[test]
    fn label_precedence() {
        assert_eq!(pick_account_label(Some("e"), Some("d"), "f"), "e");
        assert_eq!(pick_account_label(Some(""), Some("d"), "f"), "d");
        assert_eq!(pick_account_label(None, None, "f"), "f");
    }
}
