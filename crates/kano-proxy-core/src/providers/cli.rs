//! Port of apps/api/src/providers/cli.ts (docs/cli.md § Failover semantics "Routing
//! integration"): the custom adapters unchanged, with an injected transport that goes to the
//! provider's tunnel registry entry instead of the network. Format semantics (`openai`
//! near-passthrough / `anthropic` native passthrough, conversion paths, loop guard,
//! count_tokens sentinel) are inherited; only the transport differs.

use super::custom_anthropic::CustomAnthropicAdapter;
use super::custom_openai::CustomOpenAiAdapter;
use super::types::DynAdapter;
use crate::db::cli::CliProviderRow;
use crate::db::custom_providers::CustomProviderRow;
use crate::AppState;

/// The custom adapters read only these fields; `base_url` "" makes every path a bare suffix —
/// exactly the allowlisted suffix the CLI joins onto its one configured target base.
fn pseudo_custom_row(row: &CliProviderRow) -> CustomProviderRow {
    CustomProviderRow {
        id: row.id.clone(),
        user_id: row.user_id.clone(),
        slug: row.slug.clone(),
        name: row.name.clone(),
        format: row.format.clone(),
        base_url: String::new(),
        count_tokens_url: None,
        models_mode: "manual".into(),
        manual_models_json: row.models_json.clone(),
        sort_order: row.sort_order,
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}

/// `createCliAdapter(env, row)`. The transport is `cx.tunnels().transport(&row.id)`: a tunnel
/// fault comes back as the 502 `x-agent-fault` marker response, never a transport error,
/// exactly as the TypeScript `createAgentFetch` did. The catalog is agent-reported, never
/// pulled (docs/cli.md § Model catalog), so the inherited live `list_models` is dropped and
/// nothing ever queries the tunnel for a list the database already answers.
pub fn create_cli_adapter(cx: &AppState, row: &CliProviderRow) -> DynAdapter {
    let transport = cx.tunnels().transport(&row.id);
    let pseudo = pseudo_custom_row(row);
    if row.format == "anthropic" {
        CustomAnthropicAdapter::new(pseudo).with_transport(transport).without_list_models().into_dyn()
    } else {
        CustomOpenAiAdapter::new(pseudo).with_transport(transport).without_list_models().into_dyn()
    }
}

#[cfg(test)]
mod tests {
    use super::super::custom_openai::test_support::*;
    use super::super::types::CallExtras;
    use super::*;
    use crate::tunnel::mux::{MuxMessage, OutFrame};
    use crate::tunnel::protocol::{encode_binary_frame, CliProviderFormat, BODY_KIND_RESPONSE};
    use crate::tunnel::registry::{CommandReceiver, ConnectParams, SocketCommand, TunnelRegistry};
    use bytes::Bytes;
    use http::{HeaderMap, StatusCode};
    use serde_json::{json, Value};
    use std::time::Duration;

    fn cli_row(format: &str) -> CliProviderRow {
        CliProviderRow {
            id: "cliprov_1".into(),
            user_id: "user_1".into(),
            device_id: Some("clidev_1".into()),
            slug: if format == "anthropic" { "my-box".into() } else { "my-mac".into() },
            name: "My Mac".into(),
            format: format.into(),
            models_json: None,
            models_updated_at: None,
            model_filter_json: None,
            sort_order: 0,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    fn connect(registry: &TunnelRegistry, row: &CliProviderRow) -> (std::sync::Arc<crate::tunnel::registry::Connection>, CommandReceiver) {
        registry.connect(ConnectParams {
            user_id: row.user_id.clone(),
            provider_id: row.id.clone(),
            slug: row.slug.clone(),
            format: if row.format == "anthropic" { CliProviderFormat::Anthropic } else { CliProviderFormat::OpenAI },
            token_exp_ms: crate::app::now_ms() + 60_000,
        })
    }

    fn drain(rx: &mut CommandReceiver) -> Vec<SocketCommand> {
        let mut out = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            out.push(cmd);
        }
        out
    }

    /// The single `req` control frame the agent received.
    fn req_frame(commands: &[SocketCommand]) -> Value {
        commands
            .iter()
            .filter_map(|c| match c {
                SocketCommand::Frame(OutFrame::Text(text)) => serde_json::from_str::<Value>(text).ok(),
                _ => None,
            })
            .find(|v| v.get("t").and_then(Value::as_str) == Some("req"))
            .expect("a req frame")
    }

    fn sent_body(commands: &[SocketCommand]) -> Value {
        let bytes: Vec<u8> = commands
            .iter()
            .filter_map(|c| match c {
                SocketCommand::Frame(OutFrame::Binary(data)) => Some(data[5..].to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        serde_json::from_slice(&bytes).expect("a JSON request body")
    }

    /// Answers the agent's open request with a 200 JSON body.
    async fn answer(conn: &std::sync::Arc<crate::tunnel::registry::Connection>, body: &str) {
        conn.mux()
            .handle_message(MuxMessage::Text(
                json!({"t":"res","id":1,"status":200,"headers":{"content-type":"application/json"}}).to_string(),
            ))
            .await;
        conn.mux()
            .handle_message(MuxMessage::Binary(Bytes::from(encode_binary_frame(1, BODY_KIND_RESPONSE, body.as_bytes()))))
            .await;
        conn.mux().handle_message(MuxMessage::Text(json!({"t":"res_end","id":1}).to_string())).await;
    }

    #[tokio::test]
    async fn the_catalog_is_agent_reported_so_the_live_list_models_is_dropped() {
        let registry = TunnelRegistry::new();
        let state = tunnel_state(registry);
        for format in ["openai", "anthropic"] {
            let adapter = create_cli_adapter(&state, &cli_row(format));
            assert!(!adapter.has_list_models(), "{format} keeps no live catalog");
        }
    }

    #[tokio::test]
    async fn an_openai_cli_provider_has_no_count_tokens_so_the_route_answers_the_stub() {
        let registry = TunnelRegistry::new();
        let state = tunnel_state(registry);
        assert!(!create_cli_adapter(&state, &cli_row("openai")).has_count_tokens());
        // The anthropic format derives count_tokens from its (empty) base, as the custom
        // adapter does.
        assert!(create_cli_adapter(&state, &cli_row("anthropic")).has_count_tokens());
    }

    #[tokio::test]
    async fn the_adapter_slug_is_the_providers_slug() {
        let registry = TunnelRegistry::new();
        let state = tunnel_state(registry);
        assert_eq!(create_cli_adapter(&state, &cli_row("openai")).id(), "my-mac");
        assert_eq!(create_cli_adapter(&state, &cli_row("anthropic")).id(), "my-box");
    }

    #[tokio::test]
    async fn sends_only_the_bare_suffix_with_the_auth_header_stripped() {
        let registry = TunnelRegistry::new();
        let row = cli_row("openai");
        let (conn, mut rx) = connect(&registry, &row);
        let state = tunnel_state(registry);
        let adapter = create_cli_adapter(&state, &row);

        let call = tokio::spawn({
            let state = state.clone();
            async move {
                adapter
                    .chat_completions(
                        &state,
                        &account("placeholder"),
                        &chat_request(json!({ "model": "my-mac/llama3", "messages": [{ "role": "user", "content": "hi" }] }), "llama3"),
                        &CallExtras::default(),
                    )
                    .await
                    .map(|r| r.status())
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let commands = drain(&mut rx);
        let req = req_frame(&commands);
        assert_eq!(req["method"], "POST");
        assert_eq!(req["path"], "/chat/completions");
        // The placeholder credential's auth header never reaches the tunnel; only the
        // reduced header set is forwarded.
        assert_eq!(req["headers"], json!({ "content-type": "application/json" }));
        let body = sent_body(&commands);
        assert_eq!(body["model"], "llama3");
        assert_eq!(body["stream_options"], json!({ "include_usage": true }));

        answer(&conn, r#"{"id":"cmpl"}"#).await;
        assert_eq!(call.await.unwrap().unwrap(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_local_answer_comes_back_marked_x_agent_upstream() {
        let registry = TunnelRegistry::new();
        let row = cli_row("openai");
        let (conn, _rx) = connect(&registry, &row);
        let state = tunnel_state(registry);
        let adapter = create_cli_adapter(&state, &row);

        let call = tokio::spawn({
            let state = state.clone();
            async move {
                adapter
                    .chat_completions(
                        &state,
                        &account("placeholder"),
                        &chat_request(json!({ "model": "my-mac/llama3", "messages": [] }), "llama3"),
                        &CallExtras::default(),
                    )
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        answer(&conn, r#"{"id":"cmpl","usage":{"prompt_tokens":3,"completion_tokens":5}}"#).await;

        let res = call.await.unwrap().expect("a response");
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers().get("x-agent-upstream").unwrap(), "1");
        assert_eq!(body_json(res).await["usage"]["prompt_tokens"], 3);
    }

    #[tokio::test]
    async fn an_offline_tunnel_answers_502_with_the_fault_marker() {
        let registry = TunnelRegistry::new();
        let row = cli_row("openai");
        let state = tunnel_state(registry);
        let adapter = create_cli_adapter(&state, &row);
        let res = adapter
            .chat_completions(
                &state,
                &account("placeholder"),
                &chat_request(json!({ "model": "my-mac/llama3", "messages": [] }), "llama3"),
                &CallExtras::default(),
            )
            .await
            .expect("the agent transport never errors");
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(res.headers().get("x-agent-fault").unwrap(), "offline");
        assert_eq!(body_json(res).await, json!({ "error": { "type": "agent_fault", "reason": "offline" } }));
    }

    #[tokio::test]
    async fn a_fault_marker_survives_the_anthropic_adapters_openai_surface_error_rebuild() {
        // The cli-anthropic chatCompletions error path reconstructs the response to re-read
        // its text — the x-agent-fault header must survive that rebuild or dispatch can
        // neither fail over nor bench.
        let registry = TunnelRegistry::new();
        let row = cli_row("anthropic");
        let state = tunnel_state(registry);
        let adapter = create_cli_adapter(&state, &row);
        let res = adapter
            .chat_completions(
                &state,
                &account("placeholder"),
                &chat_request(json!({ "model": "my-box/some-model", "messages": [{ "role": "user", "content": "hi" }] }), "some-model"),
                &CallExtras::default(),
            )
            .await
            .expect("the agent transport never errors");
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(res.headers().get("x-agent-fault").unwrap(), "offline");
    }

    #[tokio::test]
    async fn an_anthropic_cli_provider_is_a_native_v1_messages_passthrough() {
        let registry = TunnelRegistry::new();
        let row = cli_row("anthropic");
        let (conn, mut rx) = connect(&registry, &row);
        let state = tunnel_state(registry);
        let adapter = create_cli_adapter(&state, &row);

        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", http::HeaderValue::from_static("2023-06-01"));
        headers.insert("anthropic-beta", http::HeaderValue::from_static("some-beta"));
        let body = json!({
            "model": "some-model",
            "max_tokens": 16,
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi", "cache_control": { "type": "ephemeral" } }] }],
        });
        let call = tokio::spawn({
            let state = state.clone();
            async move {
                adapter.messages(&state, &account("placeholder"), &body, &headers, &CallExtras::default()).await.map(|r| r.status())
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let commands = drain(&mut rx);
        let req = req_frame(&commands);
        assert_eq!(req["path"], "/v1/messages");
        // x-api-key is stripped; the reduced set carries the anthropic headers.
        assert_eq!(
            req["headers"],
            json!({ "content-type": "application/json", "anthropic-version": "2023-06-01", "anthropic-beta": "some-beta" })
        );
        let sent = sent_body(&commands);
        assert_eq!(sent["model"], "some-model");
        // cache_control passes through untouched — native passthrough semantics.
        assert_eq!(sent["messages"][0]["content"][0]["cache_control"], json!({ "type": "ephemeral" }));

        answer(&conn, r#"{"id":"msg_1"}"#).await;
        assert_eq!(call.await.unwrap().unwrap(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_path_outside_the_formats_allowlist_faults_as_protocol() {
        // An openai-format provider has no /v1/messages suffix: the registry refuses it.
        let registry = TunnelRegistry::new();
        let row = cli_row("openai");
        let (_conn, _rx) = connect(&registry, &row);
        let state = tunnel_state(registry);
        // The anthropic adapter over an openai-format tunnel is the only way to reach a
        // refused suffix; build it directly on the same provider id.
        let mut anthropic_row = row.clone();
        anthropic_row.format = "anthropic".into();
        let adapter = create_cli_adapter(&state, &anthropic_row);
        let res = adapter
            .messages(
                &state,
                &account("placeholder"),
                &json!({ "model": "some-model", "messages": [] }),
                &HeaderMap::new(),
                &CallExtras::default(),
            )
            .await
            .expect("the agent transport never errors");
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(res.headers().get("x-agent-fault").unwrap(), "protocol");
    }
}
