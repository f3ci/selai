//! Wire-level behaviour of the three protocol adapters against mock servers.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::time::Duration;

use selai_domain::{ErrorCode, ProviderProtocol};
use selai_llm::{echo_tool, AgentEvent, LlmError, Message, ToolDefinition, TurnOutcome};
use serde_json::{json, Value};
use support::{client, key, settings, sse, sse_raw, KEY};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, Request, ResponseTemplate,
};

fn tools() -> Vec<ToolDefinition> {
    vec![echo_tool()]
}

async fn run(
    protocol: ProviderProtocol,
    base: &str,
    api_key: Option<&selai_secrets::SecretValue>,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> (Result<TurnOutcome, LlmError>, Vec<AgentEvent>) {
    let (tx, mut rx) = mpsc::channel(256);
    let cancel = CancellationToken::new();
    let s = settings(protocol, base);
    let result = client()
        .run_turn(
            selai_llm::TurnRequest {
                settings: &s,
                api_key,
                messages,
                tools,
            },
            Some(&tx),
            &cancel,
        )
        .await;
    drop(tx);
    let mut events = Vec::new();
    while let Some(e) = rx.recv().await {
        events.push(e);
    }
    (result, events)
}

fn user(text: &str) -> Vec<Message> {
    vec![Message::User(text.to_owned())]
}

fn body_json(req: &Request) -> Value {
    serde_json::from_slice(&req.body).expect("request body is JSON")
}

fn position(events: &[AgentEvent], pred: impl Fn(&AgentEvent) -> bool) -> Option<usize> {
    events.iter().position(pred)
}

// ---- OpenAI Chat Completions ------------------------------------------------

async fn mount_chat(server: &MockServer, body: String) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn chat_streams_text_and_completes() {
    let server = MockServer::start().await;
    mount_chat(
        &server,
        sse(&[
            json!({"choices":[{"delta":{"content":"Hello"}}]}),
            json!({"choices":[{"delta":{"content":" world"}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":2}}),
        ]) + "data: [DONE]\n\n",
    )
    .await;
    let (result, events) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("hi"),
        &[],
    )
    .await;
    let outcome = result.expect("turn");
    assert_eq!(outcome.text, "Hello world");
    assert_eq!(outcome.finish_reason, "stop");
    assert_eq!(outcome.usage.input, Some(11));
    assert!(position(&events, |e| matches!(e, AgentEvent::Completed { .. })).is_some());
}

#[tokio::test]
async fn chat_fragmented_tool_call_becomes_one_ready_call_in_order() {
    let server = MockServer::start().await;
    mount_chat(
        &server,
        sse(&[
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"selai_echo","arguments":""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"te"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"xt\":\"hi\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ]) + "data: [DONE]\n\n",
    )
    .await;
    let (result, events) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("call it"),
        &tools(),
    )
    .await;
    let outcome = result.expect("turn");
    assert_eq!(outcome.tool_calls.len(), 1);
    assert_eq!(outcome.tool_calls[0].call_id, "call_1");
    assert_eq!(outcome.tool_calls[0].arguments, json!({"text": "hi"}));

    let start =
        position(&events, |e| matches!(e, AgentEvent::ToolCallStart { .. })).expect("start");
    let ready =
        position(&events, |e| matches!(e, AgentEvent::ToolCallReady { .. })).expect("ready");
    assert!(start < ready, "start must precede ready");
}

#[tokio::test]
async fn chat_malformed_arguments_never_execute() {
    let server = MockServer::start().await;
    mount_chat(
        &server,
        sse(&[
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"selai_echo","arguments":"{\"text\":"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ]) + "data: [DONE]\n\n",
    )
    .await;
    let (result, events) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("x"),
        &tools(),
    )
    .await;
    let err = result.expect_err("invalid JSON must fail the turn");
    assert_eq!(err.code, ErrorCode::ProviderProtocolError);
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallReady { .. })));
}

#[tokio::test]
async fn chat_stream_cut_mid_tool_call_is_an_error() {
    let server = MockServer::start().await;
    mount_chat(
        &server,
        sse(&[json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"selai_echo","arguments":"{\"text\":\"ha"}}]}}]})]),
    )
    .await;
    let (result, events) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("x"),
        &tools(),
    )
    .await;
    assert!(
        result.is_err(),
        "a stream without a finish must not complete"
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallReady { .. })));
}

#[tokio::test]
async fn chat_refuses_a_tool_that_was_not_offered() {
    let server = MockServer::start().await;
    mount_chat(
        &server,
        sse(&[
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"ssh.run_command","arguments":"{\"command\":\"id\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ]) + "data: [DONE]\n\n",
    )
    .await;
    let (result, events) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("x"),
        &tools(),
    )
    .await;
    assert_eq!(
        result.expect_err("unoffered").code,
        ErrorCode::ProviderProtocolError
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallReady { .. })));
}

#[tokio::test]
async fn key_travels_in_the_header_and_never_in_the_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[
                json!({"choices":[{"delta":{"content":"ok"}}]}),
                json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
            ]) + "data: [DONE]\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let (result, _) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("x"),
        &tools(),
    )
    .await;
    result.expect("authorized request succeeds");
    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(requests.len(), 1);
    let text = String::from_utf8_lossy(&requests[0].body);
    assert!(
        !text.contains(KEY),
        "API key must never appear in the request body"
    );
}

#[tokio::test]
async fn unauthorized_maps_to_its_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error":{"message":"bad key"}})),
        )
        .mount(&server)
        .await;
    let (result, _) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("x"),
        &[],
    )
    .await;
    assert_eq!(
        result.expect_err("401").code,
        ErrorCode::ProviderUnauthorized
    );
}

#[tokio::test]
async fn rate_limit_is_retried_before_any_output_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mount_chat(
        &server,
        sse(&[
            json!({"choices":[{"delta":{"content":"ok"}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        ]) + "data: [DONE]\n\n",
    )
    .await;
    let (result, _) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("x"),
        &[],
    )
    .await;
    assert_eq!(result.expect("retried").text, "ok");
    assert_eq!(server.received_requests().await.expect("recorded").len(), 2);
}

#[tokio::test]
async fn authentication_failures_keep_http_status_and_redacted_provider_details() {
    for status in [401, 403] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "error": {"code": "model_access_denied", "message": format!("Model is disabled for this account: {KEY} {}", "x".repeat(1000))}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (result, _) = run(
            ProviderProtocol::OpenAiChat,
            &format!("{}/v1", server.uri()),
            Some(&key()),
            &user("x"),
            &[],
        )
        .await;
        let err = result.expect_err("access denied");
        assert_eq!(err.code, ErrorCode::ProviderUnauthorized);
        assert!(err.message.contains(&format!("HTTP {status}")));
        assert!(err.message.contains("model_access_denied"));
        assert!(err.message.contains("Model is disabled"));
        assert!(!err.message.contains(KEY));
        assert!(err.message.chars().count() < 450);
    }
}

#[tokio::test]
async fn persistent_rate_limit_is_reported_after_bounded_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let (result, _) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("x"),
        &[],
    )
    .await;
    assert_eq!(result.expect_err("429").code, ErrorCode::ProviderRateLimit);
    assert_eq!(
        server.received_requests().await.expect("recorded").len(),
        3,
        "one try plus two retries"
    );
}

#[tokio::test]
async fn redirects_are_not_followed_so_the_key_stays_at_the_origin() {
    let attacker = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&attacker)
        .await;

    let origin = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/steal", attacker.uri()).as_str()),
        )
        .mount(&origin)
        .await;

    let (result, _) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", origin.uri()),
        Some(&key()),
        &user("x"),
        &[],
    )
    .await;
    assert_eq!(
        result.expect_err("redirect").code,
        ErrorCode::ProviderProtocolError
    );
    assert!(
        attacker
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "the redirect target must receive nothing"
    );
}

#[tokio::test]
async fn tool_rejection_from_the_server_is_classified() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"error":{"message":"auto tool choice requires --enable-auto-tool-choice"}}),
        ))
        .mount(&server)
        .await;
    let (result, _) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("x"),
        &tools(),
    )
    .await;
    assert_eq!(
        result.expect_err("tools rejected").code,
        ErrorCode::ProviderToolUnsupported
    );
}

#[tokio::test]
async fn error_bodies_are_redacted_and_truncated() {
    let server = MockServer::start().await;
    let long = format!("echoed {KEY} {}", "x".repeat(1000));
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string(long))
        .mount(&server)
        .await;
    let (result, _) = run(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("x"),
        &[],
    )
    .await;
    let err = result.expect_err("500");
    assert!(
        !err.message.contains(KEY),
        "key must be redacted from error text"
    );
    assert!(
        err.message.chars().count() < 400,
        "error text must be truncated"
    );
}

#[tokio::test]
async fn missing_key_for_a_remote_endpoint_is_refused_before_the_network() {
    let (result, _) = run(
        ProviderProtocol::OpenAiChat,
        "https://api.example.invalid/v1",
        None,
        &user("x"),
        &[],
    )
    .await;
    assert_eq!(
        result.expect_err("no key").code,
        ErrorCode::ProviderUnauthorized
    );
}

#[tokio::test]
async fn cancellation_stops_a_slow_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .mount(&server)
        .await;
    let cancel = CancellationToken::new();
    let s = settings(
        ProviderProtocol::OpenAiChat,
        &format!("{}/v1", server.uri()),
    );
    let key = key();
    let messages = user("x");
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });
    let started = std::time::Instant::now();
    let result = client()
        .run_turn(
            selai_llm::TurnRequest {
                settings: &s,
                api_key: Some(&key),
                messages: &messages,
                tools: &[],
            },
            None,
            &cancel,
        )
        .await;
    assert_eq!(
        result.expect_err("cancelled").code,
        ErrorCode::TaskCancelled
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "cancel must not wait for the server"
    );
}

// ---- OpenAI Responses -------------------------------------------------------

async fn mount_responses(server: &MockServer, body: String) {
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn responses_function_call_is_assembled_from_item_events() {
    let server = MockServer::start().await;
    mount_responses(
        &server,
        sse(&[
            json!({"type":"response.output_text.delta","delta":"Checking."}),
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_9","name":"selai_echo","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":1,"delta":"{\"text\":"}),
            json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":1,"delta":"\"hi\"}"}),
            json!({"type":"response.function_call_arguments.done","item_id":"fc_1","output_index":1,"arguments":"{\"text\":\"hi\"}"}),
            json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_9","name":"selai_echo","arguments":"{\"text\":\"hi\"}"}}),
            json!({"type":"response.completed","response":{"usage":{"input_tokens":30,"output_tokens":8}}}),
        ]),
    )
    .await;
    let (result, events) = run(
        ProviderProtocol::OpenAiResponses,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("x"),
        &tools(),
    )
    .await;
    let outcome = result.expect("turn");
    assert_eq!(outcome.text, "Checking.");
    assert_eq!(
        outcome.tool_calls.len(),
        1,
        "a call finished twice must be recorded once"
    );
    assert_eq!(outcome.tool_calls[0].call_id, "call_9");
    assert_eq!(outcome.tool_calls[0].arguments, json!({"text": "hi"}));
    assert_eq!(outcome.usage.output, Some(8));
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::TokenUsage { .. })));
}

#[tokio::test]
async fn responses_tool_result_is_sent_as_function_call_output() {
    let server = MockServer::start().await;
    mount_responses(
        &server,
        sse(&[
            json!({"type":"response.output_text.delta","delta":"done"}),
            json!({"type":"response.completed","response":{}}),
        ]),
    )
    .await;
    let messages = vec![
        Message::User("x".into()),
        Message::Assistant {
            text: String::new(),
            tool_calls: vec![selai_llm::ToolCall {
                call_id: "call_9".into(),
                name: "selai_echo".into(),
                arguments: json!({"text":"hi"}),
            }],
        },
        Message::ToolResult {
            call_id: "call_9".into(),
            name: "selai_echo".into(),
            content: "hi echoed".into(),
            is_error: false,
        },
    ];
    let (result, _) = run(
        ProviderProtocol::OpenAiResponses,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &messages,
        &tools(),
    )
    .await;
    assert_eq!(result.expect("turn").text, "done");
    let body = body_json(&server.received_requests().await.expect("recorded")[0]);
    let input = body["input"].as_array().expect("input");
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["call_id"], "call_9");
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["call_id"], "call_9");
    assert_eq!(input[2]["output"], "hi echoed");
    assert_eq!(body["store"], json!(false));
}

#[tokio::test]
async fn responses_failure_event_is_an_error_and_a_missing_completion_is_too() {
    let server = MockServer::start().await;
    mount_responses(
        &server,
        sse(&[
            json!({"type":"response.failed","response":{"error":{"message":"model overloaded"}}}),
        ]),
    )
    .await;
    let (result, _) = run(
        ProviderProtocol::OpenAiResponses,
        &format!("{}/v1", server.uri()),
        Some(&key()),
        &user("x"),
        &[],
    )
    .await;
    assert!(result
        .expect_err("failed")
        .message
        .contains("model overloaded"));

    let server2 = MockServer::start().await;
    mount_responses(
        &server2,
        sse(&[json!({"type":"response.output_text.delta","delta":"half"})]),
    )
    .await;
    let (result2, _) = run(
        ProviderProtocol::OpenAiResponses,
        &format!("{}/v1", server2.uri()),
        Some(&key()),
        &user("x"),
        &[],
    )
    .await;
    assert!(
        result2.is_err(),
        "no response.completed means the turn did not finish"
    );
}

// ---- Anthropic Messages -----------------------------------------------------

async fn mount_anthropic(server: &MockServer, body: String) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", KEY))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn anthropic_tool_use_assembles_partial_json_and_parallel_calls() {
    let server = MockServer::start().await;
    mount_anthropic(
        &server,
        sse_raw(&[
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20}}}",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Two calls.\"}}",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"selai_echo\",\"input\":{}}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"text\\\":\"}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"one\\\"}\"}}",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_2\",\"name\":\"selai_echo\",\"input\":{}}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"text\\\":\\\"two\\\"}\"}}",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":40}}",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}",
        ]),
    )
    .await;
    let (result, _) = run(
        ProviderProtocol::AnthropicMessages,
        &server.uri(),
        Some(&key()),
        &user("x"),
        &tools(),
    )
    .await;
    let outcome = result.expect("turn");
    assert_eq!(outcome.text, "Two calls.");
    assert_eq!(outcome.finish_reason, "tool_use");
    assert_eq!(outcome.tool_calls.len(), 2);
    assert_eq!(outcome.tool_calls[0].arguments, json!({"text": "one"}));
    assert_eq!(outcome.tool_calls[1].call_id, "toolu_2");
    assert_eq!(outcome.usage.input, Some(20));
    assert_eq!(outcome.usage.output, Some(40));
}

#[tokio::test]
async fn anthropic_invalid_partial_json_is_refused() {
    let server = MockServer::start().await;
    mount_anthropic(
        &server,
        sse_raw(&[
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"selai_echo\",\"input\":{}}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"text\\\":\"}}",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}",
        ]),
    )
    .await;
    let (result, events) = run(
        ProviderProtocol::AnthropicMessages,
        &server.uri(),
        Some(&key()),
        &user("x"),
        &tools(),
    )
    .await;
    assert_eq!(
        result.expect_err("bad json").code,
        ErrorCode::ProviderProtocolError
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallReady { .. })));
}

#[tokio::test]
async fn anthropic_round_trip_sends_tool_results_in_one_user_message() {
    let server = MockServer::start().await;
    mount_anthropic(
        &server,
        sse_raw(&[
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}",
        ]),
    )
    .await;
    let messages = vec![
        Message::System("be brief".into()),
        Message::User("x".into()),
        Message::Assistant {
            text: "calling".into(),
            tool_calls: vec![
                selai_llm::ToolCall {
                    call_id: "a".into(),
                    name: "selai_echo".into(),
                    arguments: json!({"text":"1"}),
                },
                selai_llm::ToolCall {
                    call_id: "b".into(),
                    name: "selai_echo".into(),
                    arguments: json!({"text":"2"}),
                },
            ],
        },
        Message::ToolResult {
            call_id: "a".into(),
            name: "selai_echo".into(),
            content: "1 echoed".into(),
            is_error: false,
        },
        Message::ToolResult {
            call_id: "b".into(),
            name: "selai_echo".into(),
            content: "bad".into(),
            is_error: true,
        },
    ];
    let (result, _) = run(
        ProviderProtocol::AnthropicMessages,
        &server.uri(),
        Some(&key()),
        &messages,
        &tools(),
    )
    .await;
    assert_eq!(result.expect("turn").text, "ok");

    let body = body_json(&server.received_requests().await.expect("recorded")[0]);
    assert_eq!(body["system"], "be brief");
    let wire = body["messages"].as_array().expect("messages");
    assert_eq!(
        wire.len(),
        3,
        "user, assistant, one merged user message with both results"
    );
    assert_eq!(wire[1]["content"][1]["type"], "tool_use");
    let results = wire[2]["content"].as_array().expect("results");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["tool_use_id"], "a");
    assert_eq!(results[1]["is_error"], json!(true));
    assert_eq!(body["tools"][0]["name"], "selai_echo");
    assert!(body["tools"][0]["input_schema"].is_object());
}

#[tokio::test]
async fn anthropic_error_event_is_an_error() {
    let server = MockServer::start().await;
    mount_anthropic(
        &server,
        sse_raw(&["event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}"]),
    )
    .await;
    let (result, _) = run(
        ProviderProtocol::AnthropicMessages,
        &server.uri(),
        Some(&key()),
        &user("x"),
        &[],
    )
    .await;
    assert!(result
        .expect_err("error event")
        .message
        .contains("Overloaded"));
}
