//! Port of apps/api/tests/openai_anthropic.test.ts. Streaming cases feed the converters on
//! awkward chunk boundaries, as the vitest suite does, so event names and JSON payloads have to
//! survive split reads.

use super::*;
use crate::proxy::sse_lines::chunked_bytes;
use futures::StreamExt;
use serde_json::json;

fn map(value: Value) -> Map<String, Value> {
    value.as_object().cloned().expect("object")
}

async fn collect(stream: ByteStream) -> String {
    let mut out = Vec::new();
    let mut stream = stream;
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("stream chunk"));
    }
    String::from_utf8(out).expect("utf-8")
}

/// The input every `openaiToAnthropicMessages` case starts from.
fn input(messages: Value, max_tokens: u64) -> AnthropicMessagesInput {
    AnthropicMessagesInput {
        model: "m".into(),
        messages: messages.as_array().cloned().unwrap_or_default(),
        max_tokens,
        ..AnthropicMessagesInput::default()
    }
}

// ---------------------------------------------------------------------------
// promptCacheKeyFromAnthropicMetadata
// ---------------------------------------------------------------------------

#[test]
fn returns_the_trimmed_claude_code_session_id() {
    let id = "user_ab12_account_9f8e7d6c-1a2b-3c4d-5e6f-7a8b9c0d1e2f_session_0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e";
    let body = map(json!({ "metadata": { "user_id": format!(" {id} ") } }));
    assert_eq!(prompt_cache_key_from_anthropic_metadata(&body).as_deref(), Some(id));
}

#[test]
fn yields_none_for_missing_non_string_empty_or_over_long_ids() {
    for body in [
        json!({}),
        json!({ "metadata": null }),
        json!({ "metadata": [] }),
        json!({ "metadata": {} }),
        json!({ "metadata": { "user_id": 42 } }),
        json!({ "metadata": { "user_id": "   " } }),
        json!({ "metadata": { "user_id": "x".repeat(257) } }),
    ] {
        assert_eq!(prompt_cache_key_from_anthropic_metadata(&map(body.clone())), None, "{body}");
    }
}

#[test]
fn accepts_an_id_at_exactly_the_256_char_limit() {
    let id = "x".repeat(256);
    let body = map(json!({ "metadata": { "user_id": id } }));
    assert_eq!(prompt_cache_key_from_anthropic_metadata(&body).map(|s| s.len()), Some(256));
}

// ---------------------------------------------------------------------------
// openaiToAnthropicMessages
// ---------------------------------------------------------------------------

#[test]
fn adds_no_cache_control_itself() {
    let mut req = input(json!([{ "role": "system", "content": "hi" }, { "role": "user", "content": "hello" }]), 100);
    req.model = "claude-opus-5".into();
    let body = openai_to_anthropic_messages(&req);
    assert!(!Value::Object(body.clone()).to_string().contains("cache_control"));
    assert_eq!(body["model"], json!("claude-opus-5"));
    assert_eq!(body["system"], json!("hi"));
}

#[test]
fn maps_stop_to_stop_sequences() {
    let mut req = input(json!([{ "role": "user", "content": "x" }]), 10);
    req.stop = Some(vec!["END".into()]);
    assert_eq!(openai_to_anthropic_messages(&req)["stop_sequences"], json!(["END"]));
    let bare = openai_to_anthropic_messages(&input(json!([{ "role": "user", "content": "x" }]), 10));
    assert!(!bare.contains_key("stop_sequences"));
}

#[test]
fn maps_tools() {
    let mut req = input(json!([{ "role": "user", "content": "x" }]), 10);
    req.tools = Some(json!([{
        "type": "function",
        "function": { "name": "foo", "description": "d", "parameters": { "type": "object", "properties": {} } }
    }]));
    assert_eq!(
        openai_to_anthropic_messages(&req)["tools"],
        json!([{ "name": "foo", "description": "d", "input_schema": { "type": "object", "properties": {} } }])
    );
}

#[test]
fn maps_tool_role_to_a_tool_result_user_message() {
    let req = input(
        json!([
            { "role": "user", "content": "call tool" },
            { "role": "assistant", "content": null, "tool_calls": [
                { "id": "call_1", "type": "function", "function": { "name": "lookup", "arguments": "{\"q\":\"a\"}" } }
            ]},
            { "role": "tool", "tool_call_id": "call_1", "content": "result text" }
        ]),
        10,
    );
    let body = openai_to_anthropic_messages(&req);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages[2],
        json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "call_1", "content": "result text" }] })
    );
}

#[test]
fn maps_tool_role_content_arrays_to_text_for_tool_result() {
    let req = input(
        json!([{ "role": "tool", "tool_call_id": "call_x", "content": [
            { "type": "text", "text": "part-a" }, { "type": "text", "text": "part-b" }
        ]}]),
        10,
    );
    let body = openai_to_anthropic_messages(&req);
    assert_eq!(
        body["messages"][0],
        json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "call_x", "content": "part-apart-b" }] })
    );
}

#[test]
fn maps_assistant_tool_calls_to_tool_use_blocks() {
    let req = input(
        json!([{ "role": "assistant", "content": "using tools", "tool_calls": [
            { "id": "call_a", "type": "function", "function": { "name": "search", "arguments": "{\"q\":\"cats\"}" } },
            { "id": "call_b", "type": "function", "function": { "name": "noop", "arguments": "{}" } }
        ]}]),
        10,
    );
    let body = openai_to_anthropic_messages(&req);
    assert_eq!(
        body["messages"][0],
        json!({ "role": "assistant", "content": [
            { "type": "text", "text": "using tools" },
            { "type": "tool_use", "id": "call_a", "name": "search", "input": { "q": "cats" } },
            { "type": "tool_use", "id": "call_b", "name": "noop", "input": {} }
        ]})
    );
}

#[test]
fn maps_invalid_tool_call_arguments_to_the_raw_fallback() {
    let req = input(
        json!([{ "role": "assistant", "content": "", "tool_calls": [
            { "id": "call_bad", "type": "function", "function": { "name": "f", "arguments": "not-json{" } }
        ]}]),
        10,
    );
    let body = openai_to_anthropic_messages(&req);
    assert_eq!(
        body["messages"][0]["content"],
        json!([{ "type": "tool_use", "id": "call_bad", "name": "f", "input": { "raw": "not-json{" } }])
    );
}

#[test]
fn maps_image_url_data_urls_to_base64_image_blocks() {
    let req = input(
        json!([{ "role": "user", "content": [
            { "type": "text", "text": "what is this?" },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,abc123" } }
        ]}]),
        10,
    );
    let body = openai_to_anthropic_messages(&req);
    assert_eq!(
        body["messages"][0]["content"],
        json!([
            { "type": "text", "text": "what is this?" },
            { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "abc123" } }
        ])
    );
}

#[test]
fn maps_image_url_https_urls_to_url_image_blocks() {
    let req = input(
        json!([{ "role": "user", "content": [
            { "type": "image_url", "image_url": { "url": "https://example.com/a.png" } }
        ]}]),
        10,
    );
    let body = openai_to_anthropic_messages(&req);
    assert_eq!(
        body["messages"][0]["content"],
        json!([{ "type": "image", "source": { "type": "url", "url": "https://example.com/a.png" } }])
    );
}

#[test]
fn maps_tool_choice_variants() {
    for (openai, anthropic) in [
        (json!("auto"), json!({ "type": "auto" })),
        (json!("none"), json!({ "type": "none" })),
        (json!("required"), json!({ "type": "any" })),
        (json!({ "type": "function", "function": { "name": "foo" } }), json!({ "type": "tool", "name": "foo" })),
    ] {
        let mut req = input(json!([{ "role": "user", "content": "x" }]), 1);
        req.tool_choice = Some(openai.clone());
        assert_eq!(openai_to_anthropic_messages(&req)["tool_choice"], anthropic, "{openai}");
    }
}

// ---------------------------------------------------------------------------
// anthropicToOpenAIResponse
// ---------------------------------------------------------------------------

#[test]
fn maps_text_and_usage() {
    let out = anthropic_to_openai_response(
        &map(json!({
            "id": "msg_1",
            "content": [{ "type": "text", "text": "ok" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 3, "output_tokens": 1 }
        })),
        "claude-code/claude-opus-5",
    );
    assert_eq!(out["choices"][0]["message"]["content"], json!("ok"));
    assert_eq!(out["usage"]["prompt_tokens"], json!(3));
    assert_eq!(out["usage"]["completion_tokens"], json!(1));
}

#[test]
fn maps_tool_use_blocks_to_openai_tool_calls() {
    let out = anthropic_to_openai_response(
        &map(json!({
            "id": "msg_2",
            "content": [
                { "type": "text", "text": "calling" },
                { "type": "tool_use", "id": "tu_1", "name": "search", "input": { "q": "x" } }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 2, "output_tokens": 4, "cache_read_input_tokens": 1, "cache_creation_input_tokens": 1 }
        })),
        "claude-code/m",
    );
    let choice = &out["choices"][0];
    assert_eq!(choice["finish_reason"], json!("tool_calls"));
    assert_eq!(choice["message"]["content"], json!("calling"));
    assert_eq!(
        choice["message"]["tool_calls"],
        json!([{ "id": "tu_1", "type": "function", "function": { "name": "search", "arguments": "{\"q\":\"x\"}" } }])
    );
    // 2 + 1 + 1 input-side tokens.
    assert_eq!(out["usage"]["prompt_tokens"], json!(4));
    assert_eq!(out["usage"]["completion_tokens"], json!(4));
    assert_eq!(out["usage"]["total_tokens"], json!(8));
}

#[test]
fn maps_max_tokens_stop_to_length_finish_reason() {
    let out = anthropic_to_openai_response(
        &map(json!({ "id": "msg_3", "content": [{ "type": "text", "text": "cut" }], "stop_reason": "max_tokens" })),
        "m",
    );
    assert_eq!(out["choices"][0]["finish_reason"], json!("length"));
}

#[test]
fn attaches_cached_tokens_and_cache_creation_from_upstream_usage() {
    let out = anthropic_to_openai_response(
        &map(json!({
            "id": "msg_4",
            "content": [{ "type": "text", "text": "ok" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 2, "output_tokens": 4, "cache_read_input_tokens": 1, "cache_creation_input_tokens": 6 }
        })),
        "claude-code/m",
    );
    assert_eq!(out["usage"]["prompt_tokens_details"], json!({ "cached_tokens": 1 }));
    assert_eq!(out["usage"]["cache_creation_input_tokens"], json!(6));
}

#[test]
fn attaches_cache_fields_as_zero_when_usage_was_present_without_them() {
    let out = anthropic_to_openai_response(
        &map(json!({
            "id": "msg_5",
            "content": [{ "type": "text", "text": "ok" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 3, "output_tokens": 1 }
        })),
        "claude-code/m",
    );
    assert_eq!(out["usage"]["prompt_tokens_details"], json!({ "cached_tokens": 0 }));
    assert_eq!(out["usage"]["cache_creation_input_tokens"], json!(0));
}

#[test]
fn a_message_with_no_usage_carries_no_usage_field() {
    let out = anthropic_to_openai_response(&map(json!({ "id": "msg_6", "content": [] })), "m");
    assert!(!out.contains_key("usage"));
    assert_eq!(out["choices"][0]["message"]["content"], Value::Null);
}

// ---------------------------------------------------------------------------
// anthropicSseToOpenAIStream — text
// ---------------------------------------------------------------------------

const ANTHROPIC_TEXT_SSE: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n",
    "\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
    "\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hel\"}}\n",
    "\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n",
    "\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n",
    "\n",
);

#[tokio::test]
async fn emits_deltas_and_done_at_every_chunk_size() {
    // Event names must survive chunk boundaries: `event:` and `data:` lines routinely arrive in
    // different network reads.
    for size in [ANTHROPIC_TEXT_SSE.len(), 7, 1] {
        let out = collect(anthropic_sse_to_openai_stream(chunked_bytes(ANTHROPIC_TEXT_SSE, size), "m")).await;
        assert!(out.contains("\"content\":\"hel\""), "size {size}: {out}");
        assert!(out.contains("\"content\":\"lo\""), "size {size}");
        assert!(out.contains("\"finish_reason\":\"stop\""), "size {size}");
        assert!(out.contains("data: [DONE]"), "size {size}");
    }
}

// ---------------------------------------------------------------------------
// stripCacheControl / anthropicToOpenAIChatRequest
// ---------------------------------------------------------------------------

#[test]
fn joins_multi_block_system_prompts_without_running_lines_together() {
    let out = anthropic_to_openai_chat_request(&map(json!({
        "model": "grok/m",
        "messages": [{ "role": "user", "content": "hi" }],
        "system": [
            { "type": "text", "text": "You are Claude Code." },
            { "type": "text", "text": "" },
            { "type": "text", "text": "You are an interactive agent.", "cache_control": { "type": "ephemeral" } }
        ]
    })));
    assert_eq!(
        out.messages[0],
        json!({ "role": "system", "content": "You are Claude Code.\n\nYou are an interactive agent." })
    );
}

#[test]
fn forwards_stop_sequences_as_openai_stop_dropping_empties() {
    let out = anthropic_to_openai_chat_request(&map(json!({
        "model": "grok/m",
        "messages": [{ "role": "user", "content": "hi" }],
        "stop_sequences": ["END", "", "\n\nHuman:"]
    })));
    assert_eq!(out.stop, Some(vec!["END".to_string(), "\n\nHuman:".to_string()]));
    let bare = anthropic_to_openai_chat_request(&map(
        json!({ "model": "grok/m", "messages": [{ "role": "user", "content": "hi" }] }),
    ));
    assert_eq!(bare.stop, None);
}

#[test]
fn strips_cache_control_deeply() {
    let stripped = strip_cache_control(&json!({
        "system": [{ "type": "text", "text": "s", "cache_control": { "type": "ephemeral" } }],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi", "cache_control": { "type": "ephemeral" } }] }],
        "cache_control": { "type": "ephemeral" }
    }));
    assert!(!stripped.to_string().contains("cache_control"), "{stripped}");
}

#[test]
fn converts_messages_and_drops_cache_control() {
    let out = anthropic_to_openai_chat_request(&map(json!({
        "model": "grok/grok-4.5",
        "max_tokens": 32,
        "system": [{ "type": "text", "text": "You are helpful.", "cache_control": { "type": "ephemeral" } }],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hello", "cache_control": { "type": "ephemeral" } }] }],
        "tools": [{ "name": "lookup", "description": "d", "input_schema": { "type": "object", "properties": {} } }],
        "tool_choice": { "type": "auto" }
    })));
    assert!(!json!(out.messages).to_string().contains("cache_control"));
    assert_eq!(
        out.messages,
        vec![
            json!({ "role": "system", "content": "You are helpful." }),
            json!({ "role": "user", "content": "hello" })
        ]
    );
    assert_eq!(out.max_tokens, Some(32));
    assert_eq!(
        out.tools,
        Some(json!([{ "type": "function", "function": { "name": "lookup", "description": "d", "parameters": { "type": "object", "properties": {} } } }]))
    );
    assert_eq!(out.tool_choice, Some(json!("auto")));
}

#[test]
fn maps_tool_use_and_tool_result() {
    let out = anthropic_to_openai_chat_request(&map(json!({
        "model": "grok/x",
        "max_tokens": 10,
        "messages": [
            { "role": "assistant", "content": [{ "type": "tool_use", "id": "t1", "name": "foo", "input": { "a": 1 } }] },
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "ok" }] }
        ]
    })));
    assert_eq!(
        out.messages[0]["tool_calls"],
        json!([{ "id": "t1", "type": "function", "function": { "name": "foo", "arguments": "{\"a\":1}" } }])
    );
    assert_eq!(out.messages[1], json!({ "role": "tool", "tool_call_id": "t1", "content": "ok" }));
}

// ---------------------------------------------------------------------------
// openaiToAnthropicMessage
// ---------------------------------------------------------------------------

#[test]
fn maps_a_text_completion() {
    let out = openai_to_anthropic_message(
        &map(json!({
            "id": "chatcmpl_1",
            "choices": [{ "message": { "role": "assistant", "content": "hi" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 1 }
        })),
        "grok/grok-4.5",
    );
    assert_eq!(out["type"], json!("message"));
    assert_eq!(out["role"], json!("assistant"));
    assert_eq!(out["model"], json!("grok/grok-4.5"));
    assert_eq!(out["stop_reason"], json!("end_turn"));
    assert_eq!(out["content"], json!([{ "type": "text", "text": "hi" }]));
    assert_eq!(out["usage"], json!({ "input_tokens": 3, "output_tokens": 1 }));
}

#[test]
fn maps_tool_calls_to_tool_use() {
    let out = openai_to_anthropic_message(
        &map(json!({
            "id": "c",
            "choices": [{ "message": { "role": "assistant", "content": null, "tool_calls": [
                { "id": "call_1", "type": "function", "function": { "name": "foo", "arguments": "{\"x\":2}" } }
            ]}, "finish_reason": "tool_calls" }]
        })),
        "grok/m",
    );
    assert_eq!(out["stop_reason"], json!("tool_use"));
    assert_eq!(out["content"], json!([{ "type": "tool_use", "id": "call_1", "name": "foo", "input": { "x": 2 } }]));
}

#[test]
fn subtracts_cached_tokens_into_cache_read_input_tokens() {
    let out = openai_to_anthropic_message(
        &map(json!({
            "id": "c",
            "choices": [{ "message": { "role": "assistant", "content": "hi" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 100, "completion_tokens": 40, "prompt_tokens_details": { "cached_tokens": 30 } }
        })),
        "grok/m",
    );
    assert_eq!(out["usage"], json!({ "input_tokens": 70, "output_tokens": 40, "cache_read_input_tokens": 30 }));
}

#[test]
fn leaves_usage_unchanged_when_no_cache_details_are_reported() {
    let out = openai_to_anthropic_message(
        &map(json!({
            "id": "c",
            "choices": [{ "message": { "role": "assistant", "content": "hi" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2 }
        })),
        "grok/m",
    );
    assert_eq!(out["usage"], json!({ "input_tokens": 5, "output_tokens": 2 }));
}

#[test]
fn prepends_an_unsigned_thinking_block_before_text() {
    let out = openai_to_anthropic_message(
        &map(json!({
            "id": "c1",
            "choices": [{ "message": { "role": "assistant", "content": "answer", "reasoning_content": "thinking it through" }, "finish_reason": "stop" }]
        })),
        "grok/grok-4.5",
    );
    assert_eq!(
        out["content"],
        json!([{ "type": "thinking", "thinking": "thinking it through" }, { "type": "text", "text": "answer" }])
    );
    assert!(!Value::Object(out).to_string().contains("signature"));
}

#[test]
fn omits_the_thinking_block_when_reasoning_content_is_absent_or_empty() {
    let no_field = openai_to_anthropic_message(
        &map(json!({ "id": "c2", "choices": [{ "message": { "role": "assistant", "content": "answer" }, "finish_reason": "stop" }] })),
        "grok/m",
    );
    assert_eq!(no_field["content"], json!([{ "type": "text", "text": "answer" }]));
    let empty = openai_to_anthropic_message(
        &map(json!({ "id": "c3", "choices": [{ "message": { "role": "assistant", "content": "answer", "reasoning_content": "" }, "finish_reason": "stop" }] })),
        "grok/m",
    );
    assert_eq!(empty["content"], json!([{ "type": "text", "text": "answer" }]));
}

#[test]
fn reports_completion_tokens_directly_without_double_adding_reasoning_tokens() {
    let out = openai_to_anthropic_message(
        &map(json!({
            "id": "c4",
            "choices": [{ "message": { "role": "assistant", "content": "hi", "reasoning_content": "r" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "completion_tokens_details": { "reasoning_tokens": 3 } }
        })),
        "grok/m",
    );
    assert_eq!(out["usage"], json!({ "input_tokens": 10, "output_tokens": 5 }));
}

#[test]
fn still_subtracts_cached_tokens_alongside_reasoning_token_details() {
    let out = openai_to_anthropic_message(
        &map(json!({
            "id": "c5",
            "choices": [{ "message": { "role": "assistant", "content": "hi" }, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 100, "completion_tokens": 40,
                "prompt_tokens_details": { "cached_tokens": 30 },
                "completion_tokens_details": { "reasoning_tokens": 10 }
            }
        })),
        "grok/m",
    );
    assert_eq!(out["usage"], json!({ "input_tokens": 70, "output_tokens": 40, "cache_read_input_tokens": 30 }));
}

// ---------------------------------------------------------------------------
// openaiSseToAnthropicStream
// ---------------------------------------------------------------------------

const OPENAI_SSE: &str = concat!(
    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hel\"},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n",
    "\n",
    "data: [DONE]\n",
);

const OPENAI_TOOL_SSE: &str = concat!(
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":null},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\"\"}}]},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\":\\\"a.ts\\\"}\"}}]},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
    "\n",
    "data: [DONE]\n",
);

const OPENAI_TEXT_THEN_TOOL_SSE: &str = concat!(
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_x\",\"type\":\"function\",\"function\":{\"name\":\"Bash\",\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}]},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
    "\n",
    "data: [DONE]\n",
);

const OPENAI_PARALLEL_TOOLS_SSE: &str = concat!(
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"A\",\"arguments\":\"{\\\"a\\\":1}\"}}]},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"B\",\"arguments\":\"{\\\"b\\\":2}\"}}]},\"finish_reason\":null}]}\n",
    "\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
    "\n",
    "data: [DONE]\n",
);

#[derive(Debug)]
struct Block {
    block_type: String,
    id: Option<String>,
    name: Option<String>,
    body: String,
}

/// Rebuild content blocks from the emitted SSE and assert the Anthropic invariant: blocks are
/// strictly sequential — one open at a time, dense ascending indices, never reopened.
fn parse_blocks(sse: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut by_index: HashMap<i64, usize> = HashMap::new();
    let mut open: Option<i64> = None;
    let mut event = String::new();
    let mut saw_message_start = false;
    let mut ended = false;
    for line in sse.split('\n') {
        if let Some(rest) = line.strip_prefix("event:") {
            event = rest.trim().to_string();
            continue;
        }
        let Some(rest) = line.strip_prefix("data:") else { continue };
        let json: Value = serde_json::from_str(rest.trim()).expect("emitted data line is JSON");
        match event.as_str() {
            "message_start" => saw_message_start = true,
            "message_delta" | "message_stop" => {
                assert_eq!(open, None, "message ended with a content block still open");
                ended = true;
            }
            e if e.starts_with("content_block") => {
                assert!(saw_message_start, "content block before message_start");
                assert!(!ended, "content block after the message ended");
            }
            _ => {}
        }
        let index = json.get("index").and_then(Value::as_i64).unwrap_or(-1);
        match event.as_str() {
            "content_block_start" => {
                assert_eq!(open, None, "a block started while another was still open");
                assert!(!by_index.contains_key(&index), "content block index reused");
                assert_eq!(index, blocks.len() as i64, "content block indices must be dense");
                by_index.insert(index, blocks.len());
                let block = &json["content_block"];
                blocks.push(Block {
                    block_type: block["type"].as_str().unwrap_or_default().to_string(),
                    id: block.get("id").and_then(Value::as_str).map(str::to_string),
                    name: block.get("name").and_then(Value::as_str).map(str::to_string),
                    body: String::new(),
                });
                open = Some(index);
            }
            "content_block_delta" => {
                assert_eq!(open, Some(index), "delta outside an open block");
                let delta = &json["delta"];
                let text = delta
                    .get("partial_json")
                    .or_else(|| delta.get("text"))
                    .or_else(|| delta.get("thinking"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let position = by_index[&index];
                blocks[position].body.push_str(text);
            }
            "content_block_stop" => {
                assert_eq!(open, Some(index));
                open = None;
            }
            _ => {}
        }
    }
    assert_eq!(open, None, "stream ended with an unclosed content block");
    assert!(ended, "stream ended without message_delta / message_stop");
    blocks
}

fn types(blocks: &[Block]) -> Vec<&str> {
    blocks.iter().map(|b| b.block_type.as_str()).collect()
}

fn tools(blocks: Vec<Block>) -> Vec<Block> {
    blocks.into_iter().filter(|b| b.block_type == "tool_use").collect()
}

async fn convert_openai(sse: &str, size: usize) -> String {
    collect(openai_sse_to_anthropic_stream(chunked_bytes(sse, size), "grok/m")).await
}

/// The `message_start` payload the converter emitted.
fn message_start_usage(sse: &str) -> Value {
    let marker = "event: message_start\ndata: ";
    let start = sse.find(marker).expect("message_start") + marker.len();
    let line = &sse[start..sse[start..].find('\n').map(|i| start + i).unwrap_or(sse.len())];
    let json: Value = serde_json::from_str(line).expect("message_start JSON");
    json["message"]["usage"].clone()
}

#[tokio::test]
async fn puts_a_real_input_count_on_message_start_when_usage_arrives_early() {
    // Most OpenAI-shaped upstreams report usage only on a final chunk, but some emit it on every
    // chunk with stream_options.include_usage, and an Anthropic client reads its context size off
    // that field (docs/api.md).
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":{\"prompt_tokens\":30,\"prompt_tokens_details\":{\"cached_tokens\":8}}}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let out = collect(openai_sse_to_anthropic_stream(chunked_bytes(sse, 4000), "codex/m")).await;
    // prompt_tokens is cache-inclusive; Anthropic input_tokens is not.
    let usage = message_start_usage(&out);
    assert_eq!(usage["input_tokens"], json!(22));
    assert_eq!(usage["output_tokens"], json!(0));
    assert_eq!(usage["cache_read_input_tokens"], json!(8));
}

#[tokio::test]
async fn leaves_message_start_at_zero_when_usage_only_arrives_at_the_end() {
    // The documented, measured case for codex/grok: there is no honest number to put there and
    // the stream must not be buffered to wait for one.
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":11}}\n\n",
        "data: [DONE]\n\n",
    );
    let out = collect(openai_sse_to_anthropic_stream(chunked_bytes(sse, 4000), "codex/m")).await;
    assert_eq!(message_start_usage(&out), json!({ "input_tokens": 0, "output_tokens": 0 }));
    assert!(out.contains("\"output_tokens\":11"));
}

#[tokio::test]
async fn emits_anthropic_text_deltas_and_message_stop() {
    let out = convert_openai(OPENAI_SSE, 11).await;
    assert!(out.contains("event: message_start"));
    assert!(out.contains("event: content_block_delta"));
    assert!(out.contains("\"text\":\"hel\""));
    assert!(out.contains("\"text\":\"lo\""));
    assert!(out.contains("event: message_stop"));
}

#[tokio::test]
async fn maps_streamed_tool_calls_to_tool_use_and_input_json_delta() {
    let out = collect(openai_sse_to_anthropic_stream(chunked_bytes(OPENAI_TOOL_SSE, 13), "grok/grok-4.5")).await;
    assert!(out.contains("event: content_block_start"));
    assert!(out.contains("\"type\":\"tool_use\""));
    assert!(out.contains("\"id\":\"call_1\""));
    assert!(out.contains("\"name\":\"Read\""));
    assert!(out.contains("\"type\":\"input_json_delta\""));
    assert!(out.contains("\"partial_json\":\"{\\\"path\\\"\""), "{out}");
    assert!(out.contains("\"partial_json\":\":\\\"a.ts\\\"}\""));
    assert!(out.contains("event: content_block_stop"));
    assert!(out.contains("\"stop_reason\":\"tool_use\""));
    assert!(out.contains("event: message_stop"));
}

#[tokio::test]
async fn closes_the_text_block_before_tool_use_when_both_appear() {
    let out = convert_openai(OPENAI_TEXT_THEN_TOOL_SSE, OPENAI_TEXT_THEN_TOOL_SSE.len()).await;
    let text_start = out.find("\"type\":\"text\"").expect("text block");
    let tool_start = out.find("\"type\":\"tool_use\"").expect("tool block");
    let first_stop = out.find("event: content_block_stop").expect("a stop");
    assert!(tool_start > text_start);
    assert!(first_stop > text_start);
    assert!(first_stop < tool_start);
    assert!(out.contains("\"name\":\"Bash\""));
    assert!(out.contains("\"stop_reason\":\"tool_use\""));
}

#[tokio::test]
async fn supports_parallel_tool_call_indices() {
    let out = convert_openai(OPENAI_PARALLEL_TOOLS_SSE, OPENAI_PARALLEL_TOOLS_SSE.len()).await;
    assert!(out.contains("\"id\":\"call_a\""));
    assert!(out.contains("\"id\":\"call_b\""));
    assert!(out.contains("\"name\":\"A\""));
    assert!(out.contains("\"name\":\"B\""));
    assert_eq!(out.matches("\"type\":\"tool_use\"").count(), 2);
    assert!(out.contains("\"stop_reason\":\"tool_use\""));
}

#[tokio::test]
async fn keeps_one_tool_call_in_one_block_when_text_interleaves_its_arguments() {
    // Grok 4.5 emits `content` between `arguments` fragments. Closing the tool block for that
    // text used to split one call across two blocks sharing an id.
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"file_path\\\":\\\"/repo/p\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"one moment\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"ackage.json\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let out = collect(openai_sse_to_anthropic_stream(chunked_bytes(sse, 17), "grok/grok-4.5")).await;
    let blocks = parse_blocks(&out);
    let text_bodies: Vec<String> =
        blocks.iter().filter(|b| b.block_type == "text").map(|b| b.body.clone()).collect();
    let calls = tools(blocks);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id.as_deref(), Some("call_1"));
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].body).unwrap(),
        json!({ "file_path": "/repo/package.json" })
    );
    // Text is preserved, in its own block.
    assert_eq!(text_bodies, vec!["one moment".to_string()]);
    assert!(out.contains("\"stop_reason\":\"tool_use\""));
}

#[tokio::test]
async fn flushes_buffered_text_between_two_tool_calls_in_order() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"/a\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"now the other one\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":1,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"/b\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let blocks = parse_blocks(&convert_openai(sse, 29).await);
    assert_eq!(types(&blocks), vec!["tool_use", "text", "tool_use"]);
    assert_eq!(blocks[1].body, "now the other one");
    let ids: Vec<String> = blocks.iter().filter_map(|b| b.id.clone()).collect();
    assert_eq!(ids, vec!["call_a".to_string(), "call_b".to_string()]);
}

#[tokio::test]
async fn closes_a_tool_block_and_flushes_text_when_the_stream_ends_abruptly() {
    // No finish_reason, no [DONE] — upstream connection simply drops.
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"/a\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"trailing\"}}]}\n\n",
    );
    let out = convert_openai(sse, 31).await;
    let blocks = parse_blocks(&out);
    assert_eq!(types(&blocks), vec!["tool_use", "text"]);
    assert_eq!(serde_json::from_str::<Value>(&blocks[0].body).unwrap(), json!({ "file_path": "/a" }));
    assert_eq!(blocks[1].body, "trailing");
    assert!(out.contains("\"stop_reason\":\"tool_use\""));
}

#[tokio::test]
async fn emits_a_well_formed_empty_message_when_upstream_sends_nothing_usable() {
    let out = convert_openai("data: [DONE]\n\n", 8).await;
    let blocks = parse_blocks(&out);
    assert_eq!(types(&blocks), vec!["text"]);
    assert_eq!(blocks[0].body, "");
    assert!(out.contains("event: message_start"));
    assert!(out.contains("\"stop_reason\":\"end_turn\""));
    assert!(out.contains("event: message_stop"));
}

#[tokio::test]
async fn waits_for_the_id_before_opening_a_block_when_name_arrives_first() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"Read\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_late\",\"type\":\"function\",\"function\":{\"arguments\":\"{\\\"file_path\\\":\\\"/a\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let calls = tools(parse_blocks(&convert_openai(sse, 41).await));
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id.as_deref(), Some("call_late"));
    assert_eq!(calls[0].name.as_deref(), Some("Read"));
    assert_eq!(serde_json::from_str::<Value>(&calls[0].body).unwrap(), json!({ "file_path": "/a" }));
}

#[tokio::test]
async fn still_emits_a_name_only_tool_call_as_a_zero_argument_call() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"Ping\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let out = convert_openai(sse, 43).await;
    let calls = tools(parse_blocks(&out));
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name.as_deref(), Some("Ping"));
    assert_eq!(calls[0].body, "");
    assert!(out.contains("\"stop_reason\":\"tool_use\""));
}

#[tokio::test]
async fn keeps_parallel_calls_intact_when_their_fragments_alternate() {
    // Fragments arriving 0,1,0,1,0,1 — the live call streams, the second is held back and
    // emitted whole, so neither JSON gets interleaved.
    let frag = |index: i64, patch: &str| {
        format!("data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":{index},{patch}}}]}}}}]}}\n\n")
    };
    let sse = [
        frag(0, "\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"A\",\"arguments\":\"\"}"),
        frag(1, "\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"B\",\"arguments\":\"\"}"),
        frag(0, "\"function\":{\"arguments\":\"{\\\"a\\\":\"}"),
        frag(1, "\"function\":{\"arguments\":\"{\\\"b\\\":\"}"),
        frag(0, "\"function\":{\"arguments\":\"1}\"}"),
        frag(1, "\"function\":{\"arguments\":\"2}\"}"),
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n".to_string(),
        "data: [DONE]\n\n".to_string(),
    ]
    .join("");
    let calls = tools(parse_blocks(&convert_openai(&sse, 47).await));
    assert_eq!(calls.iter().filter_map(|c| c.id.clone()).collect::<Vec<_>>(), vec!["call_a", "call_b"]);
    assert_eq!(calls.iter().filter_map(|c| c.name.clone()).collect::<Vec<_>>(), vec!["A", "B"]);
    assert_eq!(
        calls.iter().map(|c| serde_json::from_str::<Value>(&c.body).unwrap()).collect::<Vec<_>>(),
        vec![json!({ "a": 1 }), json!({ "b": 2 })]
    );
}

#[tokio::test]
async fn reports_real_upstream_usage_when_the_provider_sends_it() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1234,\"completion_tokens\":56}}\n\n",
        "data: [DONE]\n\n",
    );
    let out = convert_openai(sse, 53).await;
    parse_blocks(&out);
    assert!(out.contains("\"input_tokens\":1234"));
    assert!(out.contains("\"output_tokens\":56"));
}

#[tokio::test]
async fn subtracts_cached_tokens_into_cache_read_input_tokens_on_the_stream() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1000,\"completion_tokens\":56,\"prompt_tokens_details\":{\"cached_tokens\":300}}}\n\n",
        "data: [DONE]\n\n",
    );
    let out = convert_openai(sse, 41).await;
    parse_blocks(&out);
    assert!(out.contains("\"input_tokens\":700"));
    assert!(out.contains("\"cache_read_input_tokens\":300"));
    assert!(out.contains("\"output_tokens\":56"));
}

#[tokio::test]
async fn treats_a_re_sent_function_name_as_the_same_call() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"Read\",\"arguments\":\"\\\"/a\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let calls = tools(parse_blocks(&convert_openai(sse, 59).await));
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id.as_deref(), Some("call_1"));
    assert_eq!(serde_json::from_str::<Value>(&calls[0].body).unwrap(), json!({ "file_path": "/a" }));
}

#[tokio::test]
async fn adopts_a_late_id_for_a_call_that_opened_on_arguments_alone() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"/a\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_late\",\"type\":\"function\"}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let calls = tools(parse_blocks(&convert_openai(sse, 61).await));
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name.as_deref(), Some("Read"));
    assert_eq!(serde_json::from_str::<Value>(&calls[0].body).unwrap(), json!({ "file_path": "/a" }));
}

#[tokio::test]
async fn emits_a_well_formed_message_for_a_zero_byte_upstream_body() {
    // No [DONE], no events at all — the shape that makes clients retry silently.
    let out = convert_openai("", 8).await;
    let blocks = parse_blocks(&out);
    assert_eq!(types(&blocks), vec!["text"]);
    assert!(out.contains("event: message_start"));
    assert!(out.contains("event: message_stop"));
}

#[tokio::test]
async fn reads_usage_that_rides_on_the_finish_reason_chunk() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":8}}\n\n",
        "data: [DONE]\n\n",
    );
    let out = convert_openai(sse, 67).await;
    parse_blocks(&out);
    assert!(out.contains("\"input_tokens\":7"));
    assert!(out.contains("\"output_tokens\":8"));
}

#[tokio::test]
async fn ignores_content_that_arrives_after_finish_reason() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"late\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let out = convert_openai(sse, 37).await;
    let blocks = parse_blocks(&out);
    assert_eq!(blocks.iter().map(|b| b.body.clone()).collect::<Vec<_>>(), vec!["done".to_string()]);
    assert!(!out.contains("late"));
}

#[tokio::test]
async fn splits_a_new_tool_id_at_the_same_index_into_its_own_block() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"/a\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"/b\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let calls = tools(parse_blocks(&convert_openai(sse, 23).await));
    assert_eq!(calls.iter().filter_map(|c| c.id.clone()).collect::<Vec<_>>(), vec!["call_a", "call_b"]);
    assert_eq!(
        calls.iter().map(|c| serde_json::from_str::<Value>(&c.body).unwrap()).collect::<Vec<_>>(),
        vec![json!({ "file_path": "/a" }), json!({ "file_path": "/b" })]
    );
}

#[tokio::test]
async fn emits_parallel_tool_calls_as_sequential_non_overlapping_blocks() {
    let calls = tools(parse_blocks(&convert_openai(OPENAI_PARALLEL_TOOLS_SSE, 19).await));
    assert_eq!(calls.iter().filter_map(|c| c.id.clone()).collect::<Vec<_>>(), vec!["call_a", "call_b"]);
    assert_eq!(
        calls.iter().map(|c| serde_json::from_str::<Value>(&c.body).unwrap()).collect::<Vec<_>>(),
        vec![json!({ "a": 1 }), json!({ "b": 2 })]
    );
}

// ---- reasoning_content → thinking block ----

#[tokio::test]
async fn streams_reasoning_live_into_a_leading_thinking_block() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Let me \"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"think.\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"The answer is 4.\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let out = collect(openai_sse_to_anthropic_stream(chunked_bytes(sse, 23), "grok/grok-4.5")).await;
    let blocks = parse_blocks(&out);
    assert_eq!(types(&blocks), vec!["thinking", "text"]);
    assert_eq!(blocks[0].body, "Let me think.");
    assert_eq!(blocks[1].body, "The answer is 4.");
    // Unsigned — no signature is fabricated for a converted-provider thinking block.
    assert!(!out.contains("signature"));
}

#[tokio::test]
async fn closes_the_thinking_block_before_a_tool_use_block_opens() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"deciding\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"/a\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let blocks = parse_blocks(&collect(openai_sse_to_anthropic_stream(chunked_bytes(sse, 19), "grok/grok-4.5")).await);
    assert_eq!(types(&blocks), vec!["thinking", "tool_use"]);
    assert_eq!(blocks[0].body, "deciding");
}

#[tokio::test]
async fn buffers_reasoning_that_arrives_after_a_tool_block_is_open() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"/a\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"late reasoning\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let blocks = parse_blocks(&convert_openai(sse, 29).await);
    assert_eq!(types(&blocks), vec!["tool_use", "thinking"]);
    assert_eq!(blocks[1].body, "late reasoning");
}

#[tokio::test]
async fn counts_reasoning_text_toward_the_character_estimate() {
    // 10 chars => ceil(10/4) = 3 estimated tokens.
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"0123456789\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    assert!(convert_openai(sse, 17).await.contains("\"output_tokens\":3"));
}

#[tokio::test]
async fn reports_completion_tokens_in_the_stream_without_double_adding_reasoning_tokens() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"completion_tokens_details\":{\"reasoning_tokens\":20}}}\n\n",
        "data: [DONE]\n\n",
    );
    assert!(convert_openai(sse, 31).await.contains("\"output_tokens\":5"));
}

#[tokio::test]
async fn omits_any_thinking_block_when_no_reasoning_content_arrives() {
    let blocks = parse_blocks(&convert_openai(OPENAI_SSE, 11).await);
    assert!(!types(&blocks).contains(&"thinking"));
}

#[tokio::test]
async fn a_mid_stream_openai_error_line_becomes_an_anthropic_error_event() {
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
        "data: {\"error\":{\"message\":\"backend blew up\"}}\n\n",
    );
    let out = convert_openai(sse, 21).await;
    assert!(out.contains("\"text\":\"partial\""));
    assert!(out.contains("event: error"));
    assert!(out.contains("\"message\":\"backend blew up\""), "{out}");
    assert!(!out.contains("event: message_stop"));
}

// ---------------------------------------------------------------------------
// anthropicSseToOpenAIStream — tool_use, usage and errors
// ---------------------------------------------------------------------------

const ANTHROPIC_TOOL_SSE: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n",
    "\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"Read\",\"input\":{}}}\n",
    "\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"x\\\"}\"}}\n",
    "\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
    "\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":5}}\n",
    "\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n",
    "\n",
);

async fn convert_anthropic(sse: &str, size: usize) -> String {
    collect(anthropic_sse_to_openai_stream(chunked_bytes(sse, size), "claude-code/m")).await
}

#[tokio::test]
async fn maps_a_tool_use_stream_to_openai_tool_call_chunks() {
    let out = convert_anthropic(ANTHROPIC_TOOL_SSE, ANTHROPIC_TOOL_SSE.len()).await;
    assert!(out.contains("\"tool_calls\""));
    assert!(out.contains("\"id\":\"toolu_1\""));
    assert!(out.contains("\"name\":\"Read\""));
    assert!(out.contains("\"arguments\":\"{\\\"path\\\":\\\"x\\\"}\""), "{out}");
    assert!(out.contains("\"finish_reason\":\"tool_calls\""));
    assert!(out.contains("data: [DONE]"));
}

#[tokio::test]
async fn attaches_usage_on_the_final_chunk() {
    let sse = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":2,\"cache_creation_input_tokens\":3}}}\n",
        "\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n",
        "\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
        "\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n",
        "\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n",
        "\n",
    );
    let out = convert_anthropic(sse, 17).await;
    // 10 + 2 + 3 input-side tokens, the same summation anthropic_to_openai_response uses.
    assert!(out.contains("\"prompt_tokens\":15"));
    assert!(out.contains("\"completion_tokens\":7"));
    assert!(out.contains("\"total_tokens\":22"));
    assert!(out.contains("\"finish_reason\":\"stop\""));
    assert!(out.contains("data: [DONE]"));
    assert!(out.contains("\"prompt_tokens_details\":{\"cached_tokens\":2}"), "{out}");
    assert!(out.contains("\"cache_creation_input_tokens\":3"));
}

#[tokio::test]
async fn keeps_the_cache_inclusive_prompt_total_when_message_delta_repeats_input_tokens() {
    // Newer Anthropic API revisions repeat input-side fields on message_delta; `input_tokens`
    // there is the uncached count (2), not the total.
    let sse = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":2,\"cache_read_input_tokens\":20000,\"cache_creation_input_tokens\":1800,\"output_tokens\":1}}}\n",
        "\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":2,\"cache_read_input_tokens\":20000,\"cache_creation_input_tokens\":1800,\"output_tokens\":759}}\n",
        "\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n",
        "\n",
    );
    let out = convert_anthropic(sse, 23).await;
    assert!(out.contains("\"prompt_tokens\":21802"), "{out}");
    assert!(out.contains("\"completion_tokens\":759"));
    assert!(out.contains("\"total_tokens\":22561"));
    assert!(out.contains("\"prompt_tokens_details\":{\"cached_tokens\":20000}"));
    assert!(out.contains("\"cache_creation_input_tokens\":1800"));
}

#[tokio::test]
async fn re_sums_from_message_delta_when_only_it_carries_the_input_side_fields() {
    let sse = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n",
        "\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":5,\"cache_read_input_tokens\":100,\"output_tokens\":7}}\n",
        "\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n",
        "\n",
    );
    let out = convert_anthropic(sse, 19).await;
    assert!(out.contains("\"prompt_tokens\":105"), "{out}");
    assert!(out.contains("\"prompt_tokens_details\":{\"cached_tokens\":100}"));
}

#[tokio::test]
async fn attaches_stream_cache_fields_as_zero_when_message_start_carried_none() {
    let sse = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":10}}}\n",
        "\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n",
        "\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n",
        "\n",
    );
    let out = convert_anthropic(sse, 19).await;
    assert!(out.contains("\"prompt_tokens_details\":{\"cached_tokens\":0}"), "{out}");
    assert!(out.contains("\"cache_creation_input_tokens\":0"));
}

#[tokio::test]
async fn omits_usage_entirely_when_neither_event_reported_any() {
    let sse = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n",
        "\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n",
        "\n",
    );
    let out = convert_anthropic(sse, 13).await;
    assert!(!out.contains("\"usage\""), "{out}");
    assert!(out.contains("data: [DONE]"));
}

#[tokio::test]
async fn converts_a_mid_stream_error_event_into_an_openai_error_line() {
    let sse = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n",
        "\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n",
        "\n",
        "event: error\n",
        "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n",
        "\n",
    );
    let out = convert_anthropic(sse, 19).await;
    assert!(out.contains("\"content\":\"partial\""));
    assert!(out.contains("data: {\"error\":{\"message\":\"Overloaded\",\"type\":\"overloaded_error\"}}"), "{out}");
    assert!(!out.contains("[DONE]"));
    assert!(!out.contains("\"finish_reason\":\"stop\""));
}

#[tokio::test]
async fn falls_back_to_a_generic_message_and_type_when_the_error_event_omits_them() {
    let sse = "event: error\ndata: {\"type\":\"error\"}\n\n";
    let out = convert_anthropic(sse, 5).await;
    assert!(out.contains("data: {\"error\":{\"message\":\"upstream error\",\"type\":\"api_error\"}}"), "{out}");
}

// ---------------------------------------------------------------------------
// anthropicToOpenAIChatRequest: server-side tool dropping
// ---------------------------------------------------------------------------

fn chat_request(extra: Value) -> AnthropicToOpenAiChat {
    let mut body = map(json!({ "model": "grok/m", "messages": [{ "role": "user", "content": "hi" }] }));
    for (key, value) in map(extra) {
        body.insert(key, value);
    }
    anthropic_to_openai_chat_request(&body)
}

#[test]
fn drops_an_anthropic_server_side_tool() {
    let out = chat_request(json!({ "tools": [{ "type": "web_search_20250305", "name": "web_search", "max_uses": 5 }] }));
    assert_eq!(out.tools, Some(json!([])));
}

#[test]
fn still_converts_a_custom_tool_with_input_schema() {
    let out = chat_request(json!({ "tools": [{
        "type": "custom", "name": "lookup", "description": "d",
        "input_schema": { "type": "object", "properties": { "q": { "type": "string" } } }
    }]}));
    assert_eq!(
        out.tools,
        Some(json!([{ "type": "function", "function": {
            "name": "lookup", "description": "d",
            "parameters": { "type": "object", "properties": { "q": { "type": "string" } } }
        }}]))
    );
}

#[test]
fn still_converts_a_client_tool_with_no_type_field() {
    let out = chat_request(json!({ "tools": [{ "name": "lookup", "description": "d" }] }));
    assert_eq!(
        out.tools,
        Some(json!([{ "type": "function", "function": {
            "name": "lookup", "description": "d", "parameters": { "type": "object", "properties": {} }
        }}]))
    );
}

#[test]
fn passes_an_already_openai_shaped_tool_through_unchanged() {
    let tool = json!({ "type": "function", "function": { "name": "lookup", "parameters": { "type": "object", "properties": {} } } });
    let out = chat_request(json!({ "tools": [tool.clone()] }));
    assert_eq!(out.tools, Some(json!([tool])));
}

#[test]
fn drops_server_tools_alongside_a_client_tool_in_the_same_request() {
    let out = chat_request(json!({ "tools": [
        { "type": "bash_20250124", "name": "bash" },
        { "name": "lookup", "input_schema": { "type": "object", "properties": {} } }
    ]}));
    assert_eq!(
        out.tools,
        Some(json!([{ "type": "function", "function": {
            "name": "lookup", "parameters": { "type": "object", "properties": {} }
        }}]))
    );
}

// ---------------------------------------------------------------------------
// anthropicToOpenAIChatRequest: tool_result images
// ---------------------------------------------------------------------------

#[test]
fn leaves_a_text_only_tool_result_unchanged() {
    let out = chat_request(json!({ "messages": [{ "role": "user", "content": [
        { "type": "tool_result", "tool_use_id": "t1", "content": "plain text result" }
    ]}]}));
    assert_eq!(out.messages, vec![json!({ "role": "tool", "tool_call_id": "t1", "content": "plain text result" })]);
}

#[test]
fn re_attaches_a_single_image_as_a_follow_up_user_message() {
    let out = chat_request(json!({ "messages": [{ "role": "user", "content": [{
        "type": "tool_result", "tool_use_id": "t1", "content": [
            { "type": "text", "text": "here is a screenshot" },
            { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "abc123" } }
        ]
    }]}]}));
    assert_eq!(
        out.messages,
        vec![
            json!({ "role": "tool", "tool_call_id": "t1", "content": "here is a screenshot\n[image attached below]" }),
            json!({ "role": "user", "content": [
                { "type": "text", "text": "[Image(s) from tool result t1]" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,abc123" } }
            ]})
        ]
    );
}

#[test]
fn counts_multiple_images_in_the_placeholder_and_forwards_every_image_part() {
    let out = chat_request(json!({ "messages": [{ "role": "user", "content": [{
        "type": "tool_result", "tool_use_id": "t2", "content": [
            { "type": "image", "source": { "type": "url", "url": "https://example.com/a.png" } },
            { "type": "image", "source": { "type": "url", "url": "https://example.com/b.png" } }
        ]
    }]}]}));
    assert_eq!(out.messages[0], json!({ "role": "tool", "tool_call_id": "t2", "content": "[2 images attached below]" }));
    assert_eq!(
        out.messages[1]["content"],
        json!([
            { "type": "text", "text": "[Image(s) from tool result t2]" },
            { "type": "image_url", "image_url": { "url": "https://example.com/a.png" } },
            { "type": "image_url", "image_url": { "url": "https://example.com/b.png" } }
        ])
    );
}

// ---------------------------------------------------------------------------
// anthropicToOpenAIChatRequest: reasoning_effort / output_config.effort
// ---------------------------------------------------------------------------

#[test]
fn falls_back_to_output_config_effort_when_reasoning_effort_is_absent() {
    assert_eq!(chat_request(json!({ "output_config": { "effort": "high" } })).reasoning_effort, Some(json!("high")));
}

#[test]
fn prefers_an_explicit_reasoning_effort_over_output_config_effort() {
    let out = chat_request(json!({ "reasoning_effort": "low", "output_config": { "effort": "high" } }));
    assert_eq!(out.reasoning_effort, Some(json!("low")));
}

#[test]
fn leaves_reasoning_effort_unset_when_neither_field_is_present() {
    assert_eq!(chat_request(json!({})).reasoning_effort, None);
}

#[test]
fn ignores_a_non_string_output_config_effort() {
    assert_eq!(chat_request(json!({ "output_config": { "effort": 5 } })).reasoning_effort, None);
}

#[test]
fn never_reads_thinking_budget_tokens_as_a_reasoning_effort() {
    let out = chat_request(json!({ "thinking": { "type": "enabled", "budget_tokens": 4096 } }));
    assert_eq!(out.reasoning_effort, None);
    assert!(!json!(out.messages).to_string().contains("budget_tokens"));
}

#[test]
fn includes_name_response_inside_json_schema() {
    let out = chat_request(json!({ "output_format": { "type": "json_schema", "schema": { "type": "object", "properties": {} } } }));
    assert_eq!(
        out.response_format,
        Some(json!({ "type": "json_schema", "json_schema": { "name": "response", "schema": { "type": "object", "properties": {} } } }))
    );
}

#[test]
fn copies_numeric_temperature_and_top_p_verbatim() {
    let out = chat_request(json!({ "temperature": 0.3, "top_p": 0.8 }));
    assert_eq!(out.temperature, Some(0.3));
    assert_eq!(out.top_p, Some(0.8));
}

#[test]
fn omits_temperature_and_top_p_when_absent_or_non_numeric() {
    let out = chat_request(json!({ "temperature": "high" }));
    assert_eq!(out.temperature, None);
    assert_eq!(out.top_p, None);
    let bare = chat_request(json!({}));
    assert_eq!(bare.temperature, None);
    assert_eq!(bare.top_p, None);
}

// ---------------------------------------------------------------------------
// openaiToAnthropicMessages: sampling
// ---------------------------------------------------------------------------

fn sampling(temperature: Option<f64>, top_p: Option<f64>, thinking: Option<Value>, effort: Option<Value>) -> Map<String, Value> {
    let mut req = input(json!([{ "role": "user", "content": "x" }]), 10);
    req.temperature = temperature;
    req.top_p = top_p;
    req.thinking = thinking;
    req.output_config = effort;
    openai_to_anthropic_messages(&req)
}

#[test]
fn clamps_an_openai_temperature_above_anthropics_ceiling() {
    assert_eq!(sampling(Some(1.4), None, None, None)["temperature"], json!(1.0));
}

#[test]
fn leaves_an_in_range_temperature_unchanged() {
    assert_eq!(sampling(Some(0.5), None, None, None)["temperature"], json!(0.5));
}

#[test]
fn clamps_a_negative_temperature_up_to_zero() {
    assert_eq!(sampling(Some(-0.2), None, None, None)["temperature"], json!(0.0));
}

#[test]
fn passes_top_p_through_unclamped_and_omits_both_when_absent() {
    let with_top_p = sampling(None, Some(0.9), None, None);
    assert_eq!(with_top_p["top_p"], json!(0.9));
    assert!(!with_top_p.contains_key("temperature"));
    let bare = sampling(None, None, None, None);
    assert!(!bare.contains_key("temperature"));
    assert!(!bare.contains_key("top_p"));
}

#[test]
fn drops_temperature_when_an_effort_turned_thinking_on() {
    let body = sampling(Some(0.5), None, None, Some(json!({ "effort": "high" })));
    assert!(!body.contains_key("temperature"));
}

#[test]
fn keeps_temperature_when_thinking_is_explicitly_disabled() {
    let body = sampling(Some(0.5), None, Some(json!({ "type": "disabled" })), Some(json!({ "effort": "low" })));
    assert_eq!(body["temperature"], json!(0.5));
}

#[test]
fn drops_an_out_of_range_top_p_under_thinking_but_keeps_one_in_range() {
    let low = sampling(None, Some(0.5), None, Some(json!({ "effort": "high" })));
    assert!(!low.contains_key("top_p"));
    let allowed = sampling(None, Some(0.97), None, Some(json!({ "effort": "high" })));
    assert_eq!(allowed["top_p"], json!(0.97));
}

#[test]
fn leaves_sampling_untouched_when_there_is_no_thinking_config() {
    let body = sampling(Some(0.5), Some(0.5), None, None);
    assert_eq!(body["temperature"], json!(0.5));
    assert_eq!(body["top_p"], json!(0.5));
}

// ---------------------------------------------------------------------------
// thinking → reasoning_content round-trip
// ---------------------------------------------------------------------------

#[test]
fn concatenates_thinking_blocks_in_order_into_reasoning_content() {
    let out = chat_request(json!({ "messages": [{ "role": "assistant", "content": [
        { "type": "thinking", "thinking": "step one. " },
        { "type": "thinking", "thinking": "step two." },
        { "type": "text", "text": "answer" }
    ]}]}));
    assert_eq!(
        out.messages[0],
        json!({ "role": "assistant", "content": "answer", "reasoning_content": "step one. step two." })
    );
}

#[test]
fn drops_redacted_thinking_blocks() {
    let out = chat_request(json!({ "messages": [{ "role": "assistant", "content": [
        { "type": "redacted_thinking", "data": "opaque" },
        { "type": "text", "text": "answer" }
    ]}]}));
    assert_eq!(out.messages[0], json!({ "role": "assistant", "content": "answer" }));
    assert!(!json!(out.messages).to_string().contains("reasoning_content"));
}

#[test]
fn includes_reasoning_content_alongside_tool_calls_with_no_text() {
    let out = chat_request(json!({ "messages": [{ "role": "assistant", "content": [
        { "type": "thinking", "thinking": "deciding which tool" },
        { "type": "tool_use", "id": "t1", "name": "Read", "input": { "file_path": "/a" } }
    ]}]}));
    assert_eq!(
        out.messages[0],
        json!({
            "role": "assistant",
            "content": null,
            "reasoning_content": "deciding which tool",
            "tool_calls": [{ "id": "t1", "type": "function", "function": { "name": "Read", "arguments": "{\"file_path\":\"/a\"}" } }]
        })
    );
}

#[test]
fn does_not_synthesize_reasoning_content_for_plain_string_assistant_content() {
    let out = chat_request(json!({ "messages": [{ "role": "assistant", "content": "just text" }] }));
    assert_eq!(out.messages[0], json!({ "role": "assistant", "content": "just text" }));
}

#[test]
fn omits_reasoning_content_when_there_are_no_thinking_blocks() {
    let out = chat_request(json!({ "messages": [{ "role": "assistant", "content": [{ "type": "text", "text": "hi" }] }] }));
    assert_eq!(out.messages[0], json!({ "role": "assistant", "content": "hi" }));
}

// ---------------------------------------------------------------------------
// structured output field naming
// ---------------------------------------------------------------------------

#[test]
fn sends_output_config_format_never_the_retired_output_format() {
    let mut req = input(json!([{ "role": "user", "content": "x" }]), 10);
    req.output_config = Some(json!({ "effort": "high" }));
    req.response_format = Some(json!({
        "type": "json_schema",
        "json_schema": { "name": "verdict", "schema": { "type": "object", "properties": { "ok": { "type": "boolean" } } } }
    }));
    let body = openai_to_anthropic_messages(&req);
    assert!(!body.contains_key("output_format"));
    assert_eq!(
        body["output_config"],
        json!({ "effort": "high", "format": { "type": "json_schema", "schema": { "type": "object", "properties": { "ok": { "type": "boolean" } } } } })
    );
}

#[test]
fn reads_output_config_format_and_prefers_it_over_output_format() {
    let out = chat_request(json!({
        "output_config": { "format": { "type": "json_schema", "schema": { "type": "object", "properties": { "a": {} } } } },
        "output_format": { "type": "json_schema", "schema": { "type": "object", "properties": { "legacy": {} } } }
    }));
    assert_eq!(
        out.response_format,
        Some(json!({ "type": "json_schema", "json_schema": { "name": "response", "schema": { "type": "object", "properties": { "a": {} } } } }))
    );
}

#[test]
fn move_retired_output_format_relocates_only_when_no_current_spelling_exists() {
    let with_legacy = map(json!({
        "model": "m",
        "output_format": { "type": "json_schema", "schema": { "type": "object" } },
        "output_config": { "effort": "low" }
    }));
    let moved = move_retired_output_format(&with_legacy);
    assert_eq!(
        Value::Object(moved.into_owned()),
        json!({ "model": "m", "output_config": { "effort": "low", "format": { "type": "json_schema", "schema": { "type": "object" } } } })
    );
    let with_both = map(json!({
        "output_format": { "type": "json_schema", "schema": { "type": "object", "properties": { "old": {} } } },
        "output_config": { "format": { "type": "json_schema", "schema": { "type": "object" } } }
    }));
    let kept = move_retired_output_format(&with_both);
    assert_eq!(
        Value::Object(kept.into_owned()),
        json!({ "output_config": { "format": { "type": "json_schema", "schema": { "type": "object" } } } })
    );
    let untouched = map(json!({ "model": "m", "messages": [] }));
    assert!(matches!(move_retired_output_format(&untouched), Cow::Borrowed(_)));
}

// ---------------------------------------------------------------------------
// addConversionCacheControl
// ---------------------------------------------------------------------------

fn markers(value: &Value) -> Vec<Value> {
    let mut found = Vec::new();
    fn walk(value: &Value, found: &mut Vec<Value>) {
        match value {
            Value::Array(items) => items.iter().for_each(|v| walk(v, found)),
            Value::Object(map) => {
                for (key, v) in map {
                    if key == "cache_control" {
                        found.push(v.clone());
                    } else {
                        walk(v, found);
                    }
                }
            }
            _ => {}
        }
    }
    walk(value, &mut found);
    found
}

#[test]
fn spends_all_four_markers_on_an_agent_turn() {
    let one_hour = json!({ "type": "ephemeral", "ttl": "1h" });
    let out = add_conversion_cache_control(
        &map(json!({
            "system": [{ "type": "text", "text": "prepend" }, { "type": "text", "text": "instructions" }],
            "tools": [{ "name": "a", "input_schema": {} }, { "name": "b", "input_schema": {} }],
            "messages": [
                { "role": "user", "content": "first" },
                { "role": "assistant", "content": [{ "type": "tool_use", "id": "t1", "name": "a", "input": {} }] },
                { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "r1" }] },
                { "role": "assistant", "content": [{ "type": "tool_use", "id": "t2", "name": "b", "input": {} }] },
                { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t2", "content": "r2" }] }
            ]
        })),
        true,
    );
    let out = Value::Object(out);
    assert!(out["tools"][0].get("cache_control").is_none());
    assert_eq!(out["tools"][1]["cache_control"], one_hour);
    assert!(out["system"][0].get("cache_control").is_none());
    assert_eq!(out["system"][1]["cache_control"], one_hour);
    // Last message tail and the previous user turn's tail; assistant turns untouched.
    assert_eq!(out["messages"][4]["content"][0]["cache_control"], one_hour);
    assert_eq!(out["messages"][2]["content"][0]["cache_control"], one_hour);
    assert!(!out["messages"][3].to_string().contains("cache_control"));
    assert!(!out["messages"][1].to_string().contains("cache_control"));
    assert_eq!(out["messages"][0]["content"], json!("first"));
    assert_eq!(markers(&out).len(), 4);
}

#[test]
fn without_a_client_prompt_cache_key_the_message_tails_fall_back_to_five_minutes() {
    let one_hour = json!({ "type": "ephemeral", "ttl": "1h" });
    let five_min = json!({ "type": "ephemeral" });
    let out = Value::Object(add_conversion_cache_control(
        &map(json!({
            "system": "sys",
            "tools": [{ "name": "a", "input_schema": {} }],
            "messages": [
                { "role": "user", "content": "q1" },
                { "role": "assistant", "content": "a1" },
                { "role": "user", "content": "q2" }
            ]
        })),
        false,
    ));
    assert_eq!(out["tools"][0]["cache_control"], one_hour);
    // A string system is promoted to a single marked text block.
    assert_eq!(out["system"], json!([{ "type": "text", "text": "sys", "cache_control": one_hour }]));
    assert_eq!(out["messages"][2]["content"], json!([{ "type": "text", "text": "q2", "cache_control": five_min }]));
    assert_eq!(out["messages"][0]["content"], json!([{ "type": "text", "text": "q1", "cache_control": five_min }]));
    assert_eq!(out["messages"][1]["content"], json!("a1"));
    assert_eq!(markers(&out).len(), 4);
}

#[test]
fn walks_back_past_thinking_blocks_to_the_last_cacheable_block() {
    let out = Value::Object(add_conversion_cache_control(
        &map(json!({ "messages": [
            { "role": "user", "content": "q" },
            { "role": "assistant", "content": [
                { "type": "text", "text": "t" },
                { "type": "thinking", "thinking": "", "signature": "sig" }
            ]}
        ]})),
        true,
    ));
    let last = &out["messages"][1]["content"];
    assert_eq!(last[0]["cache_control"], json!({ "type": "ephemeral", "ttl": "1h" }));
    assert!(last[1].get("cache_control").is_none());
}

#[test]
fn a_single_user_message_gets_one_marker_and_no_static_markers() {
    let out = add_conversion_cache_control(&map(json!({ "messages": [{ "role": "user", "content": "only" }] })), true);
    assert!(!out.contains_key("system"));
    assert!(!out.contains_key("tools"));
    assert_eq!(markers(&Value::Object(out)).len(), 1);
}

#[test]
fn does_not_mutate_the_input_body() {
    let input = map(json!({
        "system": "sys",
        "tools": [{ "name": "a", "input_schema": {} }],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "q" }] }]
    }));
    let snapshot = Value::Object(input.clone()).to_string();
    add_conversion_cache_control(&input, true);
    assert_eq!(Value::Object(input).to_string(), snapshot);
}

// ---------------------------------------------------------------------------
// Converter flow control and line bounds (apps/api/tests/stream_memory.test.ts)
// ---------------------------------------------------------------------------

/// A never-ending upstream that counts how many chunks it was asked for.
fn counting_source(frame: &'static str) -> (ByteStream, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let served = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = served.clone();
    let stream = futures::stream::repeat_with(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(bytes::Bytes::from_static(frame.as_bytes()))
    });
    (Box::pin(stream), served)
}

const CHAT_DELTA: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
const ANTHROPIC_DELTA: &str =
    "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n";

#[tokio::test]
async fn converters_stop_reading_when_the_client_pauses() {
    use std::sync::atomic::Ordering;
    for (name, frame, convert) in [
        (
            "openai-to-anthropic",
            CHAT_DELTA,
            (|body| openai_sse_to_anthropic_stream(body, "test")) as fn(ByteStream) -> ByteStream,
        ),
        ("anthropic-to-openai", ANTHROPIC_DELTA, |body| anthropic_sse_to_openai_stream(body, "test")),
    ] {
        let (source, served) = counting_source(frame);
        let mut out = convert(source);
        out.next().await.expect("a first frame").expect("no error");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let paused = served.load(Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(paused, served.load(Ordering::SeqCst), "{name} kept draining upstream while paused");
        // Dropping the converted stream cancels the pump, which drops the upstream body.
        drop(out);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let after_cancel = served.load(Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(after_cancel, served.load(Ordering::SeqCst), "{name} kept reading after cancel");
    }
}

#[tokio::test]
async fn converters_fail_an_oversized_event_rather_than_emitting_a_false_completion() {
    use crate::proxy::sse_lines::is_sse_line_too_large;
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (name, convert) in [
        ("openai-to-anthropic", (|body| openai_sse_to_anthropic_stream(body, "test")) as fn(ByteStream) -> ByteStream),
        ("anthropic-to-openai", |body| anthropic_sse_to_openai_stream(body, "test")),
    ] {
        let blocks = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = blocks.clone();
        let oversized = bytes::Bytes::from(vec![b'A'; 64 * 1024]);
        let source: ByteStream = Box::pin(futures::stream::repeat_with(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(oversized.clone())
        }));
        let mut out = convert(source);
        let first = out.next().await.unwrap_or_else(|| panic!("{name} never failed"));
        let error = match first {
            Ok(_) => panic!("{name} emitted output for an oversized event"),
            Err(err) => err,
        };
        assert!(is_sse_line_too_large(&error), "{name}: {error}");
        // 16 MiB / 64 KiB, plus the chunk that crosses the limit.
        assert!(blocks.load(Ordering::SeqCst) <= 258, "{name}: {}", blocks.load(Ordering::SeqCst));
    }
}
