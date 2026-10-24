//! HTTP transport for one model turn.
//!
//! Redirects are never followed, so the API key cannot reach another origin.
//! Retries happen only before any body arrives, and never after output has
//! started or a tool call was returned.

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{redirect::Policy, StatusCode};
use selai_domain::{BaseUrl, ErrorCode, ProviderProtocol, ProviderSettings};
use selai_secrets::SecretValue;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    adapters::{self, Decoder},
    error::sanitize,
    sse::SseParser,
    AgentEvent, LlmError, Message, ToolDefinition, TurnOutcome, Usage,
};

const MAX_RETRIES: u32 = 2;
const ANTHROPIC_VERSION: &str = "2023-06-01";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Use HTTP(S) proxy settings from the environment. Tests turn this off so
    /// requests to a local mock server are not routed through a proxy.
    pub use_system_proxy: bool,
    pub retry_backoff: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            use_system_proxy: true,
            retry_backoff: Duration::from_millis(500),
        }
    }
}

pub struct LlmClient {
    http: reqwest::Client,
    retry_backoff: Duration,
}

/// Inputs for one turn. The key is borrowed, never copied into a `String`.
pub struct TurnRequest<'a> {
    pub settings: &'a ProviderSettings,
    pub api_key: Option<&'a SecretValue>,
    pub messages: &'a [Message],
    pub tools: &'a [ToolDefinition],
}

impl LlmClient {
    pub fn new(options: ClientOptions) -> Result<Self, LlmError> {
        let mut builder = reqwest::Client::builder()
            .redirect(Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(concat!("selai/", env!("CARGO_PKG_VERSION")));
        if !options.use_system_proxy {
            builder = builder.no_proxy();
        }
        let http = builder.build().map_err(|_| {
            LlmError::new(ErrorCode::Internal, "the HTTP client could not be created")
        })?;
        Ok(Self {
            http,
            retry_backoff: options.retry_backoff,
        })
    }

    /// Runs one model turn and streams normalized events to `sink`, if given.
    pub async fn run_turn(
        &self,
        req: TurnRequest<'_>,
        sink: Option<&mpsc::Sender<AgentEvent>>,
        cancel: &CancellationToken,
    ) -> Result<TurnOutcome, LlmError> {
        let s = req.settings;
        let key_text = req
            .api_key
            .and_then(|k| std::str::from_utf8(k.expose_for_transport()).ok());
        if req.api_key.is_some() && key_text.is_none() {
            return Err(LlmError::new(
                ErrorCode::ProviderUnauthorized,
                "the saved API key is not valid text",
            ));
        }
        if req.api_key.is_none() && !s.base_url.is_loopback() {
            return Err(LlmError::new(
                ErrorCode::ProviderUnauthorized,
                "no API key is saved for this provider",
            ));
        }
        let max = s.max_output_tokens.tokens();
        let body = match s.protocol {
            ProviderProtocol::OpenAiChat => {
                adapters::chat::body(s.model_id.as_str(), max, req.messages, req.tools)
            }
            ProviderProtocol::OpenAiResponses => {
                adapters::responses::body(s.model_id.as_str(), max, req.messages, req.tools)
            }
            ProviderProtocol::AnthropicMessages => {
                adapters::anthropic::body(s.model_id.as_str(), max, req.messages, req.tools)
            }
        };
        let url = adapters::endpoint(s.protocol, s.base_url.as_str());
        let response = self
            .send_with_retry(s, key_text, &url, &body, cancel)
            .await?;

        let allowed = req.tools.iter().map(|t| t.name.clone()).collect();
        let mut decoder = Decoder::new(s.protocol, allowed);
        let mut parser = SseParser::default();
        let mut stream = response.bytes_stream();
        let idle = Duration::from_secs(u64::from(s.timeout.seconds()));

        while !decoder.acc.finished {
            let next = tokio::select! {
                () = cancel.cancelled() => {
                    return Err(LlmError::new(ErrorCode::TaskCancelled, "the request was cancelled"));
                }
                r = tokio::time::timeout(idle, stream.next()) => r.map_err(|_| {
                    LlmError::new(ErrorCode::ProviderTimeout, "the provider stopped sending data")
                })?,
            };
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(|e| {
                if e.is_timeout() {
                    LlmError::new(ErrorCode::ProviderTimeout, "the provider stream timed out")
                } else {
                    LlmError::protocol("the provider stream was interrupted")
                }
            })?;
            for event in parser.push(&chunk)? {
                let mut out = Vec::new();
                decoder.feed(&event, &mut out)?;
                emit(sink, out).await?;
                if decoder.acc.finished {
                    break;
                }
            }
        }
        if !decoder.acc.finished {
            if let Some(event) = parser.finish() {
                let mut out = Vec::new();
                decoder.feed(&event, &mut out)?;
                emit(sink, out).await?;
            }
        }
        let acc = decoder.acc;
        if !(acc.finished || acc.finish_reason.is_some()) {
            return Err(LlmError::protocol(
                "the stream ended before the provider finished its reply",
            ));
        }
        if acc.assembler.has_open() {
            return Err(LlmError::protocol(
                "the provider ended the reply with an open tool call",
            ));
        }
        let finish_reason = acc
            .finish_reason
            .clone()
            .unwrap_or_else(|| "stop".to_owned());
        let usage = acc.usage;
        let mut tail = Vec::new();
        if usage != Usage::default() {
            tail.push(AgentEvent::TokenUsage {
                input: usage.input,
                output: usage.output,
            });
        }
        tail.push(AgentEvent::Completed {
            finish_reason: finish_reason.clone(),
        });
        emit(sink, tail).await?;
        Ok(TurnOutcome {
            text: acc.text,
            tool_calls: acc.calls,
            finish_reason,
            usage,
        })
    }

    /// Lists the model IDs the endpoint offers (`GET {base}/models`, or
    /// `{base}/v1/models` for Anthropic). A 401 or 403 becomes
    /// `PROVIDER_UNAUTHORIZED`, but a public catalog does not verify chat access.
    /// One attempt, no retries, and the
    /// body is capped so a misbehaving server cannot exhaust memory.
    pub async fn list_models(
        &self,
        protocol: ProviderProtocol,
        base_url: &BaseUrl,
        api_key: Option<&SecretValue>,
        timeout: Duration,
    ) -> Result<Vec<String>, LlmError> {
        let key = api_key.and_then(|k| std::str::from_utf8(k.expose_for_transport()).ok());
        if api_key.is_some() && key.is_none() {
            return Err(LlmError::new(
                ErrorCode::ProviderUnauthorized,
                "the API key is not valid text",
            ));
        }
        if key.is_none() && !base_url.is_loopback() {
            return Err(LlmError::new(
                ErrorCode::ProviderUnauthorized,
                "enter an API key first; it is needed for a provider that is not on this machine",
            ));
        }
        let base = base_url.as_str();
        let url = match protocol {
            ProviderProtocol::AnthropicMessages => format!("{base}/v1/models?limit=1000"),
            _ => format!("{base}/models"),
        };
        let mut request = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(k) = key {
            request = match protocol {
                ProviderProtocol::AnthropicMessages => request
                    .header("x-api-key", k)
                    .header("anthropic-version", ANTHROPIC_VERSION),
                _ => request.bearer_auth(k),
            };
        }
        let response = match tokio::time::timeout(timeout, request.send()).await {
            Err(_) => {
                return Err(LlmError::new(
                    ErrorCode::ProviderTimeout,
                    "the provider did not answer in time",
                ))
            }
            Ok(Err(e)) if e.is_timeout() => {
                return Err(LlmError::new(
                    ErrorCode::ProviderTimeout,
                    "the provider did not answer in time",
                ))
            }
            Ok(Err(_)) => {
                return Err(LlmError::protocol(
                    "could not reach the provider; check the base URL and network",
                ))
            }
            Ok(Ok(r)) => r,
        };
        let status = response.status();
        let body = read_capped(response, MODELS_BODY_LIMIT, timeout).await?;
        if !status.is_success() {
            if status.as_u16() == 404 {
                return Err(LlmError::protocol(
                    "this endpoint does not list models (HTTP 404); type the model ID instead",
                ));
            }
            return Err(status_error(status, &String::from_utf8_lossy(&body), key));
        }
        let value: Value = serde_json::from_slice(&body)
            .map_err(|_| LlmError::protocol("the model list is not valid JSON"))?;
        Ok(model_ids(&value))
    }

    async fn send_with_retry(
        &self,
        s: &ProviderSettings,
        key: Option<&str>,
        url: &str,
        body: &Value,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response, LlmError> {
        let deadline = Duration::from_secs(u64::from(s.timeout.seconds()));
        let mut attempt: u32 = 0;
        loop {
            let mut request = self
                .http
                .post(url)
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .json(body);
            if let Some(k) = key {
                request = match s.protocol {
                    ProviderProtocol::AnthropicMessages => request
                        .header("x-api-key", k)
                        .header("anthropic-version", ANTHROPIC_VERSION),
                    _ => request.bearer_auth(k),
                };
            }
            let sent = tokio::select! {
                () = cancel.cancelled() => {
                    return Err(LlmError::new(ErrorCode::TaskCancelled, "the request was cancelled"));
                }
                r = tokio::time::timeout(deadline, request.send()) => r,
            };
            let response = match sent {
                Err(_) => {
                    return Err(LlmError::new(
                        ErrorCode::ProviderTimeout,
                        "the provider did not answer in time",
                    ));
                }
                Ok(Err(err)) => {
                    if err.is_timeout() {
                        return Err(LlmError::new(
                            ErrorCode::ProviderTimeout,
                            "the provider did not answer in time",
                        ));
                    }
                    if attempt < MAX_RETRIES {
                        self.backoff(attempt, cancel).await?;
                        attempt += 1;
                        continue;
                    }
                    return Err(LlmError::protocol(
                        "could not reach the provider; check the base URL and network",
                    ));
                }
                Ok(Ok(r)) => r,
            };
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            if is_retryable(status) && attempt < MAX_RETRIES {
                self.backoff(attempt, cancel).await?;
                attempt += 1;
                continue;
            }
            let text = response.text().await.unwrap_or_default();
            return Err(status_error(status, &text, key));
        }
    }

    async fn backoff(&self, attempt: u32, cancel: &CancellationToken) -> Result<(), LlmError> {
        let wait = self.retry_backoff * (attempt + 1);
        tokio::select! {
            () = cancel.cancelled() => Err(LlmError::new(ErrorCode::TaskCancelled, "the request was cancelled")),
            () = tokio::time::sleep(wait) => Ok(()),
        }
    }
}

const MODELS_BODY_LIMIT: usize = 4 * 1024 * 1024;
const MODELS_MAX: usize = 1000;

async fn read_capped(
    response: reqwest::Response,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>, LlmError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        let next = tokio::time::timeout(timeout, stream.next())
            .await
            .map_err(|_| {
                LlmError::new(
                    ErrorCode::ProviderTimeout,
                    "the provider stopped sending data",
                )
            })?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|_| LlmError::protocol("the provider reply was interrupted"))?;
        if body.len() + chunk.len() > limit {
            return Err(LlmError::protocol("the model list is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Model IDs from an OpenAI-style (`data[].id`), Anthropic (`data[].id`) or
/// Ollama-native (`models[].name`) list. Sorted, without duplicates or blanks.
pub(crate) fn model_ids(value: &Value) -> Vec<String> {
    let from = |list: &Value, field: &str| -> Vec<String> {
        list.as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|m| m[field].as_str().or_else(|| m.as_str()))
                    .map(str::trim)
                    .filter(|id| !id.is_empty() && id.len() <= 256)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut ids = from(&value["data"], "id");
    if ids.is_empty() {
        ids = from(&value["models"], "name");
    }
    ids.sort();
    ids.dedup();
    ids.truncate(MODELS_MAX);
    ids
}

fn is_retryable(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 503 | 504)
}

/// Maps an HTTP failure to a stable code. The body is sanitized before it is
/// placed in the message.
fn status_error(status: StatusCode, body: &str, key: Option<&str>) -> LlmError {
    let code = status.as_u16();
    let detail = sanitize(body, key);
    match code {
        401 | 403 => LlmError::new(
            ErrorCode::ProviderUnauthorized,
            if detail.trim().is_empty() {
                format!("the provider rejected the API key or the account lacks access (HTTP {code})")
            } else {
                format!("the provider rejected the API key or the account lacks access (HTTP {code}): {detail}")
            },
        ),
        429 => LlmError::new(ErrorCode::ProviderRateLimit, "the provider rate limit was reached; try again later"),
        300..=399 => LlmError::protocol(format!(
            "the provider answered with a redirect (HTTP {code}); redirects are not followed, so check the base URL"
        )),
        400 | 404 | 422 if body.to_ascii_lowercase().contains("tool") => LlmError::new(
            ErrorCode::ProviderToolUnsupported,
            format!("the endpoint or model does not accept tool calls: {detail}"),
        ),
        _ => LlmError::protocol(format!("the provider answered with HTTP {code}: {detail}")),
    }
}

async fn emit(
    sink: Option<&mpsc::Sender<AgentEvent>>,
    events: Vec<AgentEvent>,
) -> Result<(), LlmError> {
    let Some(sink) = sink else { return Ok(()) };
    for event in events {
        sink.send(event).await.map_err(|_| {
            LlmError::new(ErrorCode::TaskCancelled, "the consumer stopped listening")
        })?;
    }
    Ok(())
}
