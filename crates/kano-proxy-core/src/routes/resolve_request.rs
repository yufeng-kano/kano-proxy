//! Port of apps/api/src/routes/resolve_request.ts (docs/api.md § Model routing, § Group
//! endpoints).
//!
//! Model resolution shared by both surface mounts: the shared bases resolve `provider/model`
//! directly; a `/g/{slug}/…` mount resolves the slug to the caller's group first (unknown slug
//! → the route's 404) and then the request's `model` within that group. The TypeScript branched
//! on whether Hono had a `slug` param; here the group mounts pass `Some(slug)` and the shared
//! bases pass `None` — the handlers themselves stay shared.

use crate::db::model_groups::get_group_by_slug;
use crate::routing::candidates::{resolve_candidates, resolve_group_model_candidates, RoutingResolution};
use crate::AppState;

/// What `resolve_request_model` concluded. The two failure arms carry exactly what the routes
/// need for their surface-shaped envelope and their `request_logs` row.
pub enum RequestModelResolution {
    Ok(Box<RoutingResolution>),
    /// The path slug is not one of the caller's groups.
    GroupNotFound { slug: String },
    /// No such model: an unparsable `provider/model` on a shared base, or a name the group
    /// does not define (`group_slug` is `Some` only on a group mount).
    InvalidModel { group_slug: Option<String> },
}

/// `resolveRequestModel(c, userId, modelRaw)`; `slug` is the group mount's path parameter.
pub async fn resolve_request_model(
    cx: &AppState,
    user_id: &str,
    slug: Option<&str>,
    model_raw: &str,
) -> Result<RequestModelResolution, sqlx::Error> {
    let Some(slug) = slug else {
        return Ok(match resolve_candidates(cx, user_id, model_raw).await? {
            Some(resolution) => RequestModelResolution::Ok(Box::new(resolution)),
            None => RequestModelResolution::InvalidModel { group_slug: None },
        });
    };
    let Some(group) = get_group_by_slug(cx.pool(), user_id, slug).await? else {
        return Ok(RequestModelResolution::GroupNotFound { slug: slug.to_string() });
    };
    Ok(match resolve_group_model_candidates(cx, user_id, &group, model_raw).await? {
        Some(resolution) => RequestModelResolution::Ok(Box::new(resolution)),
        None => RequestModelResolution::InvalidModel { group_slug: Some(slug.to_string()) },
    })
}

/// Fixtures shared by the LLM-surface route tests (apps/api/tests/helpers + the per-test
/// `seed*` helpers in responses_routes / audio_* / *_llm_routes). Every upstream is a
/// [`MockTransport`]; no test ever reaches a real provider (docs/testing.md).
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, Response};
    use axum::Router;
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use serde_json::Value;

    use crate::db::custom_providers::{insert_custom_provider, NewCustomProvider};
    use crate::db::model_groups::{insert_model_group, replace_group_models, GroupModelInput, GroupTarget};
    use crate::db::test_support::{insert_account, insert_api_key, insert_user, test_pool, test_state};
    use crate::db::users::UserRow;
    use crate::extensions::Extensions;
    use crate::pool::StoredCredential;
    use crate::upstream::{MockTransport, UpstreamTransport};
    use crate::AppState;

    pub struct Fixture {
        pub state: AppState,
        pub mock: Arc<MockTransport>,
        pub user: UserRow,
        pub key: String,
    }

    /// A user with one project API key on a fresh database, or `None` when no test database
    /// answered (the caller then skips).
    pub async fn fixture() -> Option<Fixture> {
        let pool = test_pool().await?;
        let mock = MockTransport::new();
        let state = test_state(pool, mock.clone() as Arc<dyn UpstreamTransport>);
        let user = insert_user(state.pool(), "routes@example.com").await;
        let (_, key) = insert_api_key(state.pool(), &user.id).await;
        Some(Fixture { state, mock, user, key })
    }

    impl Fixture {
        pub fn router(&self) -> Router {
            crate::build_router(self.state.clone(), Extensions::default())
        }

        /// One bound `upstream_accounts` row for `provider`, so the pool has a candidate.
        pub async fn account(&self, provider: &str) {
            let credential = StoredCredential {
                access_token: "upstream-test-token".into(),
                ..Default::default()
            };
            insert_account(self.state.pool(), &self.user.id, provider, &credential).await;
        }

        /// One bound account whose stored credential carries `extra` — the antigravity project
        /// id and anything else an adapter reads out of the encrypted payload.
        pub async fn account_with_extra(&self, provider: &str, extra: serde_json::Map<String, Value>) {
            let credential = StoredCredential {
                access_token: "upstream-test-token".into(),
                extra: Some(extra),
                ..Default::default()
            };
            insert_account(self.state.pool(), &self.user.id, provider, &credential).await;
        }

        /// A custom provider plus its account — the BYO endpoint the conversion tests point at.
        pub async fn custom_provider(&self, slug: &str, format: &str, base_url: &str) {
            insert_custom_provider(
                self.state.pool(),
                NewCustomProvider {
                    user_id: &self.user.id,
                    slug,
                    name: slug,
                    format,
                    base_url,
                    count_tokens_url: None,
                    models_mode: "manual",
                    manual_models_json: Some("[\"local-model\"]"),
                },
            )
            .await
            .expect("insert custom provider");
            self.account(slug).await;
        }

        /// A model group at `/g/<slug>` with one model whose targets are `provider/model` ids.
        pub async fn group(&self, slug: &str, model_name: &str, targets: &[&str]) {
            let group = insert_model_group(self.state.pool(), &self.user.id, model_name, slug, None)
                .await
                .expect("insert group");
            replace_group_models(
                self.state.pool(),
                &self.user.id,
                &group.id,
                &[GroupModelInput {
                    name: model_name.to_string(),
                    targets: targets
                        .iter()
                        .map(|t| GroupTarget { model: (*t).to_string(), account_id: None })
                        .collect(),
                }],
            )
            .await
            .expect("insert group models");
        }

        pub fn post(&self, path: &str, body: Value) -> Request<Body> {
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", format!("Bearer {}", self.key))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request builds")
        }

        pub fn get(&self, path: &str) -> Request<Body> {
            Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {}", self.key))
                .body(Body::empty())
                .expect("request builds")
        }

        /// A `multipart/form-data` body with the given text fields and, optionally, a `file`.
        pub fn multipart(&self, path: &str, fields: &[(&str, &str)], file: Option<(&str, &[u8])>) -> Request<Body> {
            let boundary = "----kano-test-boundary";
            let mut out: Vec<u8> = Vec::new();
            for (name, value) in fields {
                out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
                out.extend_from_slice(format!("content-disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes());
                out.extend_from_slice(value.as_bytes());
                out.extend_from_slice(b"\r\n");
            }
            if let Some((file_name, bytes)) = file {
                out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
                out.extend_from_slice(
                    format!(
                        "content-disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\ncontent-type: audio/mpeg\r\n\r\n"
                    )
                    .as_bytes(),
                );
                out.extend_from_slice(bytes);
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", format!("Bearer {}", self.key))
                .header("content-type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(out))
                .expect("request builds")
        }

        /// Every `request_logs` row once at least `expected` have been written — the rows are
        /// spawned off the response path, exactly as `waitUntil` wrote them.
        pub async fn logs(&self, expected: usize) -> Vec<Value> {
            for _ in 0..100 {
                let rows: Vec<Value> =
                    sqlx::query_scalar("SELECT row_to_json(r) FROM request_logs r ORDER BY created_at, ctid")
                        .fetch_all(self.state.pool())
                        .await
                        .expect("read request_logs");
                if rows.len() >= expected {
                    return rows;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            panic!("only saw fewer than {expected} request_logs rows");
        }
    }

    pub async fn body_bytes(response: Response<Body>) -> Bytes {
        response.into_body().collect().await.expect("body collects").to_bytes()
    }

    pub async fn body_json(response: Response<Body>) -> Value {
        serde_json::from_slice(&body_bytes(response).await).expect("body is JSON")
    }

    pub async fn body_text(response: Response<Body>) -> String {
        String::from_utf8(body_bytes(response).await.to_vec()).expect("body is UTF-8")
    }

    /// Reads an SSE body chunk by chunk (never buffered by the test helper itself) and returns
    /// the concatenated text.
    pub async fn drain_sse(response: Response<Body>) -> String {
        use futures::StreamExt;
        let mut stream = response.into_body().into_data_stream();
        let mut out = String::new();
        while let Some(chunk) = stream.next().await {
            out.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
        }
        out
    }

    /// The JSON payload of every `data:` line in an SSE body.
    pub fn sse_events(text: &str) -> Vec<Value> {
        text.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .filter(|payload| !payload.is_empty() && *payload != "[DONE]")
            .map(|payload| serde_json::from_str(payload).expect("SSE data is JSON"))
            .collect()
    }

}

#[cfg(test)]
mod tests {
    use super::test_support::fixture;
    use super::*;
    use crate::db::test_support::skip_without_db;

    #[tokio::test]
    async fn a_shared_base_resolves_provider_slash_model_and_rejects_a_bare_id() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("claude-code").await;

        let resolved = resolve_request_model(&f.state, &f.user.id, None, "claude-code/claude-opus-5").await.unwrap();
        let RequestModelResolution::Ok(resolution) = resolved else { panic!("expected a resolution") };
        assert_eq!(resolution.primary.provider, "claude-code");
        assert_eq!(resolution.primary.upstream_model, "claude-opus-5");
        assert!(resolution.group_name.is_none());

        let bare = resolve_request_model(&f.state, &f.user.id, None, "claude-opus-5").await.unwrap();
        assert!(matches!(bare, RequestModelResolution::InvalidModel { group_slug: None }));
    }

    #[tokio::test]
    async fn a_group_mount_resolves_the_slug_first_then_the_name_as_an_exact_match() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("claude-code").await;
        f.group("team", "fast", &["claude-code/claude-opus-5"]).await;

        let resolved = resolve_request_model(&f.state, &f.user.id, Some("team"), "fast").await.unwrap();
        let RequestModelResolution::Ok(resolution) = resolved else { panic!("expected a resolution") };
        assert_eq!(resolution.group_name.as_deref(), Some("team/fast"));
        assert_eq!(resolution.primary.provider, "claude-code");

        // An unknown slug is the route's 404; a known slug with an unknown name is
        // invalid_model — and a `provider/model` string never falls through to the shared base.
        let unknown_slug = resolve_request_model(&f.state, &f.user.id, Some("nope"), "fast").await.unwrap();
        assert!(matches!(unknown_slug, RequestModelResolution::GroupNotFound { ref slug } if slug == "nope"));
        let unknown_model = resolve_request_model(&f.state, &f.user.id, Some("team"), "claude-code/claude-opus-5")
            .await
            .unwrap();
        assert!(matches!(unknown_model, RequestModelResolution::InvalidModel { group_slug: Some(ref s) } if s == "team"));
    }

    #[tokio::test]
    async fn another_users_group_slug_is_never_visible() {
        let Some(f) = fixture().await else { return skip_without_db() };
        let other = crate::db::test_support::insert_user(f.state.pool(), "other@example.com").await;
        let group = crate::db::model_groups::insert_model_group(f.state.pool(), &other.id, "fast", "team", None)
            .await
            .unwrap();
        assert_eq!(group.slug, "team");

        let resolved = resolve_request_model(&f.state, &f.user.id, Some("team"), "fast").await.unwrap();
        assert!(matches!(resolved, RequestModelResolution::GroupNotFound { .. }));
    }
}
