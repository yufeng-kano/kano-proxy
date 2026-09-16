//! the model-group virtual endpoints
//! (docs/api.md § Group endpoints): every group is served at `/g/{slug}/openai/v1/…` and
//! `/g/{slug}/anthropic/v1/…`, mirroring the shared bases.
//!
//! The POST handlers are the shared-surface ones — they branch on the `slug` param inside
//! `resolve_request_model` — so only the per-group `/models` catalogs live here. The table is
//! closed and exact-match: an unknown slug is this module's 404, never a fallthrough to the
//! shared base's `provider/model` resolution.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};

use crate::db::model_groups::{get_group_by_slug, list_models_for_group, ModelGroupRow};
use crate::extensions::ApiKeyIdentity;
use crate::http::errors::ApiError;
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/{slug}/openai/v1/models", get(openai_models))
        .route("/{slug}/anthropic/v1/models", get(anthropic_models))
        .route("/{slug}/openai/v1/chat/completions", post(super::openai::group_chat_completions))
        .route("/{slug}/openai/v1/responses", post(super::openai::group_responses))
        .route("/{slug}/openai/v1/audio/transcriptions", post(super::openai::group_audio_transcriptions))
        .route("/{slug}/anthropic/v1/messages", post(super::anthropic::group_messages))
        .route("/{slug}/anthropic/v1/messages/count_tokens", post(super::anthropic::group_count_tokens))
}

/// The caller's group behind the path slug, or `None` (the route's 404).
async fn group_for_request(
    state: &AppState,
    id: &ApiKeyIdentity,
    slug: &str,
) -> Result<Option<ModelGroupRow>, ApiError> {
    Ok(get_group_by_slug(state.pool(), &id.user_id, slug).await?)
}

/// A group's own catalog: exactly its model names, listed regardless of current target
/// usability (docs/providers.md § Model groups "Catalog").
async fn openai_models(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    Path(slug): Path<String>,
) -> Response {
    let group = match group_for_request(&state, &id, &slug).await {
        Ok(Some(group)) => group,
        Ok(None) => {
            return ApiError::openai(
                StatusCode::NOT_FOUND,
                &format!("unknown group endpoint \"{slug}\""),
                "invalid_request_error",
                "not_found",
            )
            .into_response()
        }
        Err(err) => return err.into_response(),
    };
    let models = match list_models_for_group(state.pool(), &group.id).await {
        Ok(models) => models,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let data: Vec<Value> = models
        .iter()
        .map(|m| json!({ "id": m.name, "object": "model", "owned_by": "group", "display_name": m.name }))
        .collect();
    Json(json!({ "object": "list", "data": data })).into_response()
}

async fn anthropic_models(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    Path(slug): Path<String>,
) -> Response {
    let group = match group_for_request(&state, &id, &slug).await {
        Ok(Some(group)) => group,
        Ok(None) => {
            return ApiError::anthropic(
                StatusCode::NOT_FOUND,
                "not_found_error",
                &format!("unknown group endpoint \"{slug}\""),
            )
            .into_response()
        }
        Err(err) => return err.into_response(),
    };
    let models = match list_models_for_group(state.pool(), &group.id).await {
        Ok(models) => models,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let data: Vec<Value> =
        models.iter().map(|m| json!({ "id": m.name, "display_name": m.name, "type": "model" })).collect();
    Json(json!({ "data": data })).into_response()
}

#[cfg(test)]
mod tests {
    use super::super::resolve_request::test_support::{body_json, fixture, Fixture};
    use crate::db::test_support::skip_without_db;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    const BASE_URL: &str = "https://upstream.example.com/v1";

    async fn seeded() -> Option<Fixture> {
        let f = fixture().await?;
        f.custom_provider("mygw", "openai", BASE_URL).await;
        f.group("team", "fast", &["mygw/local-model"]).await;
        Some(f)
    }

    /// The table is closed: every route exists for a known slug and 404s for an unknown one,
    /// in the envelope of its own surface (docs/api.md § Group endpoints).
    #[tokio::test]
    async fn every_group_route_404s_for_an_unknown_slug_in_its_surface_envelope() {
        let Some(f) = seeded().await else { return skip_without_db() };

        for path in [
            "/g/nope/openai/v1/models",
            "/g/nope/openai/v1/chat/completions",
            "/g/nope/openai/v1/responses",
            "/g/nope/openai/v1/audio/transcriptions",
        ] {
            let request = if path.ends_with("models") {
                f.get(path)
            } else if path.ends_with("transcriptions") {
                f.multipart(path, &[("model", "fast")], Some(("clip.mp3", b"ID3")))
            } else {
                f.post(path, json!({ "model": "fast", "messages": [] }))
            };
            let response = f.router().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            let json = body_json(response).await;
            assert_eq!(json["error"]["message"], "unknown group endpoint \"nope\"", "{path}");
        }

        for path in ["/g/nope/anthropic/v1/models", "/g/nope/anthropic/v1/messages", "/g/nope/anthropic/v1/messages/count_tokens"] {
            let request =
                if path.ends_with("models") { f.get(path) } else { f.post(path, json!({ "model": "fast", "messages": [] })) };
            let response = f.router().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            let json = body_json(response).await;
            assert_eq!(json["type"], "error", "{path}");
            assert_eq!(json["error"]["type"], "not_found_error", "{path}");
        }
    }

    /// A slug is resolved scoped to the key's owner — another user's group is a 404, exactly
    /// like a cross-user custom slug on the shared base.
    #[tokio::test]
    async fn another_users_slug_is_a_404_not_a_borrowed_endpoint() {
        let Some(f) = fixture().await else { return skip_without_db() };
        let other = crate::db::test_support::insert_user(f.state.pool(), "neighbour@example.com").await;
        let group = crate::db::model_groups::insert_model_group(f.state.pool(), &other.id, "fast", "theirs", None)
            .await
            .unwrap();
        crate::db::model_groups::replace_group_models(
            f.state.pool(),
            &other.id,
            &group.id,
            &[crate::db::model_groups::GroupModelInput {
                name: "fast".into(),
                targets: vec![crate::db::model_groups::GroupTarget { model: "mygw/local-model".into(), account_id: None }],
            }],
        )
        .await
        .unwrap();

        let response = f.router().oneshot(f.get("/g/theirs/openai/v1/models")).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response = f
            .router()
            .oneshot(f.post("/g/theirs/openai/v1/chat/completions", json!({ "model": "fast", "messages": [] })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// The group is a closed mapping table: a `provider/model` string that resolves fine on
    /// the shared base does not fall through here.
    #[tokio::test]
    async fn a_group_never_falls_through_to_the_shared_bases_provider_model_resolution() {
        let Some(f) = seeded().await else { return skip_without_db() };

        let response = f
            .router()
            .oneshot(f.post("/g/team/openai/v1/chat/completions", json!({ "model": "mygw/local-model", "messages": [] })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"]["code"], "invalid_model");
        assert_eq!(f.mock.requests().len(), 0);
    }

    #[tokio::test]
    async fn the_group_audio_mount_resolves_the_name_and_forwards_to_its_target() {
        let Some(f) = seeded().await else { return skip_without_db() };
        f.mock.expect(|_| {
            Ok(crate::upstream::UpstreamResponse::json(StatusCode::OK, &json!({ "text": "hello there" })))
        });

        let response = f
            .router()
            .oneshot(f.multipart(
                "/g/team/openai/v1/audio/transcriptions",
                &[("model", "fast")],
                Some(("clip.mp3", b"ID3")),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["text"], "hello there");
        let calls = f.mock.requests();
        assert_eq!(calls[0].url, format!("{BASE_URL}/audio/transcriptions"));
        let sent = String::from_utf8_lossy(calls[0].body.as_deref().unwrap()).to_string();
        assert!(sent.contains("local-model"));

        let rows = f.logs(1).await;
        assert_eq!(rows[0]["model"], "mygw/local-model");
        assert_eq!(rows[0]["group_name"], "team/fast");
    }

    /// Auth is unchanged on the group mounts: no key, no endpoint.
    #[tokio::test]
    async fn the_group_mounts_authenticate_exactly_like_the_shared_bases() {
        let Some(f) = seeded().await else { return skip_without_db() };
        let anonymous = |path: &str| {
            Request::builder().method("POST").uri(path).body(Body::from("{}")).unwrap()
        };

        let response = f.router().oneshot(anonymous("/g/team/openai/v1/chat/completions")).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(response).await["error"]["code"], "invalid_api_key");

        let response = f.router().oneshot(anonymous("/g/team/anthropic/v1/messages")).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(response).await["error"]["type"], "authentication_error");

        let response = f
            .router()
            .oneshot(Request::builder().uri("/g/team/openai/v1/models").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
