//! Capability probes for a configured provider. They run no SSH and no local
//! command. A tool probe passes only when the model makes a real tool call with
//! arguments, and then gives a final answer after receiving the tool result.

use std::time::Instant;

use selai_domain::{ErrorCode, ProviderSettings};
use selai_secrets::SecretValue;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    client::{LlmClient, ToolChoice, TurnRequest},
    error::sanitize,
    LlmError, Message, ToolDefinition,
};

pub const PROBE_TOOL: &str = "selai_echo";
pub const PROBE_TOKEN: &str = "selai-probe-7f3a";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionTest {
    pub latency_ms: u64,
    pub reply_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolProbe {
    pub verified: bool,
    /// Why the probe did not pass, when it did not.
    pub code: Option<ErrorCode>,
    pub detail: String,
    pub latency_ms: u64,
}

pub fn echo_tool() -> ToolDefinition {
    ToolDefinition {
        name: PROBE_TOOL.to_owned(),
        description: "Echoes the text argument back. Used only to test tool calling.".to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
            "additionalProperties": false
        }),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub async fn test_connectivity(
    client: &LlmClient,
    settings: &ProviderSettings,
    key: Option<&SecretValue>,
    cancel: &CancellationToken,
) -> Result<ConnectionTest, LlmError> {
    let started = Instant::now();
    let messages = [Message::User("Reply with the single word: ok".to_owned())];
    let outcome = client
        .run_turn(
            TurnRequest {
                settings,
                api_key: key,
                messages: &messages,
                tools: &[],
            },
            None,
            cancel,
        )
        .await?;
    if outcome.text.trim().is_empty() {
        return Err(LlmError::protocol("the provider replied without any text"));
    }
    let key_text = key.and_then(|k| std::str::from_utf8(k.expose_for_transport()).ok());
    let preview: String = sanitize(outcome.text.trim(), key_text)
        .chars()
        .take(80)
        .collect();
    Ok(ConnectionTest {
        latency_ms: elapsed_ms(started),
        reply_preview: preview,
    })
}

fn not_verified(code: ErrorCode, detail: impl Into<String>, started: Instant) -> ToolProbe {
    ToolProbe {
        verified: false,
        code: Some(code),
        detail: detail.into(),
        latency_ms: elapsed_ms(started),
    }
}

pub async fn probe_tools(
    client: &LlmClient,
    settings: &ProviderSettings,
    key: Option<&SecretValue>,
    cancel: &CancellationToken,
) -> Result<ToolProbe, LlmError> {
    let started = Instant::now();
    let tools = [echo_tool()];
    let first = Message::User(format!(
        "This is a two-step tool test. In this response, call the {PROBE_TOOL} tool exactly once with text set to {PROBE_TOKEN} and return only the tool call. After I send the tool result, reply in one short plain-text sentence confirming that result. Do not call any tools again."
    ));
    let first_messages = [first.clone()];

    let turn = client
        .run_turn_with_tool_choice(
            TurnRequest {
                settings,
                api_key: key,
                messages: &first_messages,
                tools: &tools,
            },
            None,
            cancel,
            ToolChoice::Required,
        )
        .await;
    let outcome = match turn {
        Ok(o) => o,
        Err(e)
            if matches!(
                e.code,
                ErrorCode::ProviderToolUnsupported | ErrorCode::ProviderProtocolError
            ) =>
        {
            return Ok(not_verified(e.code, e.message, started));
        }
        Err(e) => return Err(e),
    };
    let Some(call) = outcome.tool_calls.iter().find(|c| c.name == PROBE_TOOL) else {
        return Ok(not_verified(
            ErrorCode::ProviderToolUnsupported,
            "the model replied without calling the tool",
            started,
        ));
    };
    let echoed = call.arguments["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if !echoed.contains("selai-probe") {
        return Ok(not_verified(
            ErrorCode::ProviderProtocolError,
            "the tool call did not carry the requested text argument",
            started,
        ));
    }

    // Return the tool result as the model would receive it in a real task.
    let second_messages = [
        first,
        Message::Assistant {
            text: outcome.text.clone(),
            tool_calls: vec![call.clone()],
        },
        Message::ToolResult {
            call_id: call.call_id.clone(),
            name: PROBE_TOOL.to_owned(),
            content: format!("{echoed} echoed"),
            is_error: false,
        },
    ];
    let final_turn = client
        .run_turn(
            TurnRequest {
                settings,
                api_key: key,
                messages: &second_messages,
                tools: &[],
            },
            None,
            cancel,
        )
        .await;
    let final_outcome = match final_turn {
        Ok(o) => o,
        Err(e)
            if matches!(
                e.code,
                ErrorCode::ProviderToolUnsupported | ErrorCode::ProviderProtocolError
            ) =>
        {
            return Ok(not_verified(e.code, e.message, started));
        }
        Err(e) => return Err(e),
    };
    if !final_outcome.tool_calls.is_empty() {
        return Ok(not_verified(
            ErrorCode::ProviderProtocolError,
            "the model called a tool again instead of answering after the result",
            started,
        ));
    }
    if final_outcome.text.trim().is_empty() {
        return Ok(not_verified(
            ErrorCode::ProviderProtocolError,
            "no final answer followed the tool result",
            started,
        ));
    }
    Ok(ToolProbe {
        verified: true,
        code: None,
        detail: "tool call and tool result round trip completed".to_owned(),
        latency_ms: elapsed_ms(started),
    })
}
