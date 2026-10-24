//! Provider flows through the application service, against mock providers.
//! No SSH is involved. These run on every OS because they need no sshd.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use selai_desktop_lib::{
    chat_service::{ChatEventView, ChatMessageInput},
    dto::{ProviderInput, ProviderModelsInput, ProviderTestView},
    service::Services,
};
use selai_domain::ErrorCode;
use selai_llm::{ClientOptions, LlmClient};
use selai_secrets::{MemoryStore, SecretError};
use serde_json::{json, Value};
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, Request, Respond, ResponseTemplate,
};

const KEY: &str = "sentinel-provider-key-never-leak";

fn sse(payloads: &[Value]) -> String {
    payloads
        .iter()
        .map(|p| format!("data: {p}\n\n"))
        .collect::<String>()
        + "data: [DONE]\n\n"
}

/// A model that supports tools: a tool call first, then text after the result.
struct ToolCapableChat;

impl Respond for ToolCapableChat {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).expect("json");
        let has_result = body["messages"]
            .as_array()
            .is_some_and(|m| m.iter().any(|x| x["role"] == "tool"));
        let payload = if has_result {
            sse(&[
                json!({"choices":[{"delta":{"content":"done"}}]}),
                json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
            ])
        } else {
            sse(&[
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"selai_echo","arguments":"{\"text\":\"selai-probe-7f3a\"}"}}]}}]}),
                json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            ])
        };
        ResponseTemplate::new(200).set_body_raw(payload, "text/event-stream")
    }
}

struct Harness {
    services: Services,
    store: Arc<MemoryStore>,
    _dir: tempfile::TempDir,
    db_path: std::path::PathBuf,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("selai.sqlite3");
    let store = Arc::new(MemoryStore::new());
    let llm = LlmClient::new(ClientOptions {
        use_system_proxy: false,
        retry_backoff: std::time::Duration::from_millis(5),
    })
    .expect("client");
    let services = Services::open(&db_path, store.clone())
        .expect("open")
        .with_llm_client(llm);
    Harness {
        services,
        store,
        _dir: dir,
        db_path,
    }
}

fn input(base: &str, model: &str, api_key: Option<&str>) -> ProviderInput {
    ProviderInput {
        id: None,
        expected_updated_at_ms: None,
        name: "local-vllm".to_owned(),
        protocol: "openai_chat".to_owned(),
        base_url: base.to_owned(),
        model_id: model.to_owned(),
        timeout_seconds: 10,
        max_output_tokens: 256,
        api_key: api_key.map(str::to_owned),
        clear_api_key: None,
        allow_insecure_http: None,
    }
}

async fn tool_test(h: &Harness, id: &str) -> ProviderTestView {
    h.services
        .provider_test(id, "tool_calling")
        .await
        .expect("test runs")
}

#[tokio::test]
async fn key_is_stored_outside_the_database_and_never_returned() {
    let h = harness();
    let server = MockServer::start().await;
    let view = h
        .services
        .provider_save(input(
            &format!("{}/v1", server.uri()),
            "mock-model",
            Some(KEY),
        ))
        .expect("save");
    assert!(view.key_configured);
    assert_eq!(h.store.len(), 1, "key is in the OS store");

    let json = serde_json::to_string(&view).expect("json");
    assert!(!json.contains(KEY));
    let listed =
        serde_json::to_string(&h.services.provider_list(None).expect("list")).expect("json");
    assert!(!listed.contains(KEY));
    let db = std::fs::read(&h.db_path).expect("db");
    let wal = std::fs::read(h.db_path.with_extension("sqlite3-wal")).unwrap_or_default();
    assert!(!db.windows(KEY.len()).any(|w| w == KEY.as_bytes()));
    assert!(!wal.windows(KEY.len()).any(|w| w == KEY.as_bytes()));
}

#[tokio::test]
async fn tool_calling_round_trip_verifies_the_provider_and_sends_the_key_only_in_the_header() {
    let h = harness();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ToolCapableChat)
        .mount(&server)
        .await;
    let view = h
        .services
        .provider_save(input(
            &format!("{}/v1", server.uri()),
            "mock-model",
            Some(KEY),
        ))
        .expect("save");
    assert_eq!(
        view.tool_calling.status, "not_verified",
        "nothing is verified before a probe"
    );

    let result = tool_test(&h, &view.id).await;
    assert!(result.ok && result.verified, "{result:?}");
    let listed = h.services.provider_list(None).expect("list");
    assert_eq!(listed.items[0].tool_calling.status, "verified");

    for request in server.received_requests().await.expect("recorded") {
        let body = String::from_utf8_lossy(&request.body);
        assert!(
            !body.contains(KEY),
            "key must not appear in any request body"
        );
        let auth = request
            .headers
            .get("authorization")
            .map(|v| v.to_str().unwrap_or_default().to_owned());
        assert_eq!(auth.as_deref(), Some(format!("Bearer {KEY}").as_str()));
    }
}

#[tokio::test]
async fn changing_the_model_invalidates_the_verification() {
    let h = harness();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ToolCapableChat)
        .mount(&server)
        .await;
    let base = format!("{}/v1", server.uri());
    let view = h
        .services
        .provider_save(input(&base, "model-a", Some(KEY)))
        .expect("save");
    assert!(tool_test(&h, &view.id).await.verified);

    let mut changed = input(&base, "model-b", None);
    changed.id = Some(view.id.clone());
    changed.expected_updated_at_ms = Some(view.updated_at_ms);
    let after = h.services.provider_save(changed).expect("update");
    assert_eq!(
        after.tool_calling.status, "not_verified",
        "a different model must not inherit the result"
    );
    assert!(after.key_configured, "the key survives a model change");
}

#[tokio::test]
async fn bad_key_is_reported_and_leaves_the_verification_alone() {
    let h = harness();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let view = h
        .services
        .provider_save(input(
            &format!("{}/v1", server.uri()),
            "mock-model",
            Some(KEY),
        ))
        .expect("save");
    let result = tool_test(&h, &view.id).await;
    assert!(!result.ok);
    assert_eq!(result.code, Some(ErrorCode::ProviderUnauthorized));
    assert_eq!(
        h.services.provider_list(None).expect("list").items[0]
            .tool_calling
            .status,
        "not_verified"
    );
}

#[tokio::test]
async fn remote_endpoint_without_a_key_is_refused_without_a_network_call() {
    let h = harness();
    let view = h
        .services
        .provider_save(input("https://api.example.invalid/v1", "mock-model", None))
        .expect("save");
    assert!(!view.key_configured);
    let result = h
        .services
        .provider_test(&view.id, "connectivity")
        .await
        .expect("runs");
    assert!(!result.ok);
    assert_eq!(result.code, Some(ErrorCode::ProviderUnauthorized));
}

#[tokio::test]
async fn text_only_model_is_marked_failed_not_verified() {
    let h = harness();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[
                json!({"choices":[{"delta":{"content":"hello"}}]}),
                json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
            ]),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let view = h
        .services
        .provider_save(input(
            &format!("{}/v1", server.uri()),
            "text-model",
            Some(KEY),
        ))
        .expect("save");
    let result = tool_test(&h, &view.id).await;
    assert!(!result.verified);
    assert_eq!(result.code, Some(ErrorCode::ProviderToolUnsupported));
    assert_eq!(
        h.services.provider_list(None).expect("list").items[0]
            .tool_calling
            .status,
        "failed"
    );
}

#[tokio::test]
async fn locked_store_fails_closed_and_creates_no_profile() {
    let h = harness();
    h.store.fail_all_with(SecretError::Locked);
    let err = h
        .services
        .provider_save(input("http://127.0.0.1:9/v1", "mock-model", Some(KEY)))
        .expect_err("locked");
    assert_eq!(err.code, ErrorCode::SecretStoreLocked);
    assert!(h
        .services
        .provider_list(None)
        .expect("list")
        .items
        .is_empty());
}

#[tokio::test]
async fn deleting_a_provider_removes_its_key() {
    let h = harness();
    let view = h
        .services
        .provider_save(input("http://127.0.0.1:9/v1", "mock-model", Some(KEY)))
        .expect("save");
    assert_eq!(h.store.len(), 1);
    h.services.provider_delete(&view.id).expect("delete");
    assert!(h.store.is_empty());
    assert!(h
        .services
        .provider_list(None)
        .expect("list")
        .items
        .is_empty());
}

#[tokio::test]
async fn invalid_base_urls_and_models_are_rejected_before_anything_is_written() {
    let h = harness();
    for bad_url in [
        "http://example.com/v1",
        "https://u:p@example.com/v1",
        "ftp://x",
    ] {
        let err = h
            .services
            .provider_save(input(bad_url, "m", Some(KEY)))
            .expect_err(bad_url);
        assert_eq!(err.code, ErrorCode::InvalidInput, "{bad_url}");
    }
    assert_eq!(
        h.services
            .provider_save(input("https://example.com/v1", "two words", None))
            .expect_err("model")
            .code,
        ErrorCode::InvalidInput
    );
    assert!(h.store.is_empty());
}

#[test]
fn templates_offer_protocols_and_never_a_model_id() {
    let templates = Services::provider_templates();
    let ids: Vec<_> = templates.iter().map(|t| t.id).collect();
    for expected in [
        "openai",
        "anthropic",
        "zai-glm",
        "moonshot-kimi",
        "litellm",
        "vllm",
        "ollama",
        "custom",
    ] {
        assert!(ids.contains(&expected), "missing {expected}");
    }
    for t in &templates {
        assert!(matches!(
            t.protocol,
            "openai_responses" | "openai_chat" | "anthropic_messages"
        ));
        assert!(!t.note.is_empty());
    }
}

#[tokio::test]
async fn plain_http_to_another_host_needs_the_explicit_opt_in() {
    let h = harness();
    let base = "http://10.255.255.1:4000/v1";

    let refused = h
        .services
        .provider_save(input(base, "litellm-model", Some(KEY)))
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);

    let mut opted_in = input(base, "litellm-model", Some(KEY));
    opted_in.allow_insecure_http = Some(true);
    let view = h
        .services
        .provider_save(opted_in)
        .expect("saved with opt-in");
    assert!(view.allow_insecure_http);
    assert!(!view.is_loopback);
    assert_eq!(view.base_url, base);

    // The opt-in survives a reload of the list.
    let listed = h.services.provider_list(None).expect("list");
    assert!(listed
        .items
        .iter()
        .any(|p| p.id == view.id && p.allow_insecure_http));
}

#[tokio::test]
async fn check_api_key_lists_models_with_the_typed_key_or_the_saved_one() {
    let h = harness();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "qwen-tools"}, {"id": "llama-3"}]
        })))
        .expect(2)
        .mount(&server)
        .await;
    let base = format!("{}/v1", server.uri());

    // Before saving: the key typed in the form.
    let draft = ProviderModelsInput {
        id: None,
        protocol: "openai_chat".into(),
        base_url: base.clone(),
        allow_insecure_http: None,
        api_key: Some(KEY.into()),
    };
    let models = h.services.provider_models(draft).await.expect("models");
    assert_eq!(models.models, vec!["llama-3", "qwen-tools"]);

    // After saving, with the key field left blank: the saved key.
    let saved = h
        .services
        .provider_save(input(&base, "qwen-tools", Some(KEY)))
        .expect("save");
    let again = ProviderModelsInput {
        id: Some(saved.id.clone()),
        protocol: "openai_chat".into(),
        base_url: base.clone(),
        allow_insecure_http: None,
        api_key: None,
    };
    assert_eq!(
        h.services
            .provider_models(again)
            .await
            .expect("models")
            .models
            .len(),
        2
    );

    // A wrong key is a clear error, and the typed key is not in it.
    let wrong = ProviderModelsInput {
        id: None,
        protocol: "openai_chat".into(),
        base_url: base,
        allow_insecure_http: None,
        api_key: Some("wrong-key".into()),
    };
    let err = h.services.provider_models(wrong).await.expect_err("401");
    assert!(!err.message.contains("wrong-key"));
}

#[tokio::test]
async fn plain_chat_streams_a_reply_without_tools() {
    let h = harness();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("json");
            assert!(
                body.get("tools").is_none(),
                "plain chat must not offer tools"
            );
            let roles: Vec<&str> = body["messages"]
                .as_array()
                .expect("messages")
                .iter()
                .map(|m| m["role"].as_str().unwrap_or(""))
                .collect();
            assert_eq!(roles, vec!["system", "user", "assistant", "user"]);
            ResponseTemplate::new(200).set_body_raw(
                sse(&[
                    json!({"choices":[{"delta":{"content":"Use `ss -tlnp`."}}]}),
                    json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
                    json!({"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":7}}),
                ]),
                "text/event-stream",
            )
        })
        .mount(&server)
        .await;
    let saved = h
        .services
        .provider_save(input(&format!("{}/v1", server.uri()), "m", None))
        .expect("save");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let sink: Arc<dyn Fn(ChatEventView) + Send + Sync> = Arc::new(move |e| {
        let _ = tx.send(e);
    });
    let msg = |role: &str, content: &str| ChatMessageInput {
        role: role.into(),
        content: content.into(),
    };
    h.services
        .chat_send(
            &saved.id,
            &[
                msg("user", "how do I list ports?"),
                msg("assistant", "Which OS?"),
                msg("user", "Ubuntu"),
            ],
            sink,
        )
        .await
        .expect("send");

    let mut text = String::new();
    let mut usage = None;
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect("in time")
            .expect("event");
        match event {
            ChatEventView::TextDelta { text: t } => text.push_str(&t),
            ChatEventView::Usage {
                input_tokens,
                output_tokens,
            } => usage = Some((input_tokens, output_tokens)),
            ChatEventView::Completed => break,
            ChatEventView::Failed { code, message } => panic!("{code}: {message}"),
        }
    }
    assert_eq!(text, "Use `ss -tlnp`.");
    assert_eq!(usage, Some((Some(42), Some(7))));
}

#[tokio::test]
async fn public_catalog_does_not_verify_chat_access_and_test_uses_the_saved_key_and_model() {
    let h = harness();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "restricted-model"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(
            ResponseTemplate::new(403)
                .set_delay(std::time::Duration::from_millis(30))
                .set_body_json(
                    json!({"error": {"message": format!("Model access denied: {KEY}")}}),
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let base = format!("{}/v1", server.uri());
    let models = h
        .services
        .provider_models(ProviderModelsInput {
            id: None,
            protocol: "openai_chat".into(),
            base_url: base.clone(),
            allow_insecure_http: None,
            api_key: Some(KEY.into()),
        })
        .await
        .expect("public catalog loads");
    assert_eq!(models.models, vec!["restricted-model"]);
    let saved = h
        .services
        .provider_save(input(&base, "restricted-model", Some(KEY)))
        .expect("save");
    assert_eq!(saved.tool_calling.status, "not_verified");
    let result = h
        .services
        .provider_test(&saved.id, "connectivity")
        .await
        .expect("test runs");
    assert!(!result.ok);
    assert_eq!(result.code, Some(ErrorCode::ProviderUnauthorized));
    assert!(result.message.contains("HTTP 403"));
    assert!(result.message.contains("Model access denied"));
    assert!(!result.message.contains(KEY));
    assert!(result.latency_ms >= 30);
    let requests = server.received_requests().await.expect("requests");
    let post = requests
        .iter()
        .find(|request| request.method == "POST")
        .expect("chat request");
    let body: Value = serde_json::from_slice(&post.body).expect("json");
    assert_eq!(body["model"], "restricted-model");
    assert!(!String::from_utf8_lossy(&post.body).contains(KEY));
}
