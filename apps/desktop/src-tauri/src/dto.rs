//! Data shapes exchanged with the webview. Nothing here holds secret material
//! except the write-only input fields, which are never echoed back.

use selai_domain::ErrorCode;
use selai_storage::{GroupRecord, HostRecord};
use serde::{Deserialize, Serialize};

/// Error payload for every failing command: `{ code, message, details? }`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<KeyErrorDetails>,
}

/// Host-key facts attached to `SSH_HOSTKEY_UNKNOWN` and `SSH_HOSTKEY_CHANGED`
/// so the UI can show what the user is being asked to trust.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyErrorDetails {
    pub algorithm: String,
    pub fingerprint_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_fingerprint_sha256: Option<String>,
}

impl CommandError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageStatus {
    pub schema_version: i64,
    pub journal_mode: String,
    pub interrupted_tasks_at_startup: usize,
    pub interrupted_executions_at_startup: usize,
}

/// Host as the UI sees it. `credentialConfigured` is a flag, never the secret.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostView {
    pub id: String,
    pub label: String,
    pub group_id: Option<String>,
    pub proxy_id: Option<String>,
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub auth_method: &'static str,
    pub private_key_path: Option<String>,
    pub connect_timeout_seconds: u16,
    pub keep_alive_seconds: u16,
    pub terminal_type: String,
    pub notes: Option<String>,
    pub credential_configured: bool,
    pub updated_at_ms: i64,
    pub last_connected_at_ms: Option<i64>,
    pub archived: bool,
    /// `ssh`, or `local` for the native computer and WSL distributions.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wsl_distribution: Option<String>,
}

impl From<HostRecord> for HostView {
    fn from(h: HostRecord) -> Self {
        Self {
            id: h.id.to_string(),
            label: if cfg!(windows) && h.is_local && h.wsl_distribution.is_none() {
                "PowerShell".to_owned()
            } else {
                h.label
            },
            group_id: h.group_id.map(|g| g.to_string()),
            proxy_id: None,
            hostname: h.hostname,
            port: h.port,
            username: h.username,
            auth_method: h.auth.as_str(),
            private_key_path: h.private_key_path,
            connect_timeout_seconds: h.connect_timeout_seconds,
            keep_alive_seconds: h.keepalive_seconds,
            terminal_type: h.terminal_type,
            notes: h.notes,
            credential_configured: h.credential_configured,
            updated_at_ms: h.updated_at_ms,
            last_connected_at_ms: h.last_connected_at_ms,
            archived: h.archived_at_ms.is_some(),
            kind: if h.is_local { "local" } else { "ssh" },
            wsl_distribution: h.wsl_distribution,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupView {
    pub id: String,
    pub name: String,
    pub sort_order: i64,
}

impl From<GroupRecord> for GroupView {
    fn from(g: GroupRecord) -> Self {
        Self {
            id: g.id.to_string(),
            name: g.name,
            sort_order: g.sort_order,
        }
    }
}

/// Create or update request. Secrets are not returned in host metadata. Viewing
/// a saved SSH password requires the separate, explicit reveal command.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInput {
    pub id: Option<String>,
    pub expected_updated_at_ms: Option<i64>,
    pub label: String,
    pub group_id: Option<String>,
    #[serde(default)]
    pub proxy_id: Option<String>,
    pub hostname: String,
    pub port: u32,
    pub username: String,
    pub auth_method: String,
    pub private_key_path: Option<String>,
    pub connect_timeout_seconds: u32,
    pub keep_alive_seconds: u32,
    pub terminal_type: String,
    pub notes: Option<String>,
    /// Empty or absent leaves the stored password unchanged.
    pub password: Option<String>,
    /// Empty or absent leaves the stored passphrase unchanged.
    pub key_passphrase: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshKeyView {
    pub id: String,
    pub name: String,
    pub algorithm: String,
    pub public_key: String,
    pub fingerprint_sha256: String,
    pub created_at_ms: i64,
}

impl From<selai_storage::SshKeyRecord> for SshKeyView {
    fn from(k: selai_storage::SshKeyRecord) -> Self {
        Self {
            id: k.id,
            name: k.name,
            algorithm: k.algorithm,
            public_key: k.public_key,
            fingerprint_sha256: k.fingerprint_sha256,
            created_at_ms: k.created_at_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedKeyView {
    pub algorithm: String,
    pub fingerprint_sha256: String,
    pub public_key_base64: String,
}

/// Result of a host-key probe. `status` says how the key compares with pins.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeView {
    pub observed: ObservedKeyView,
    /// `trusted`, `unknown`, or `changed`.
    pub status: &'static str,
    pub pinned_fingerprint_sha256: Option<String>,
}

/// How a probed key should be stored. `first` pins a key for a host with no
/// pin. `replace` is only for an explicit key change and keeps the old key.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustMode {
    First,
    Replace,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustView {
    /// `pinned`, `rotated`, or `unchanged`.
    pub outcome: &'static str,
    pub fingerprint_sha256: String,
}

/// Events for one terminal session, delivered over a Tauri channel. Output is
/// base64 so binary bytes survive JSON intact.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TerminalEvent {
    Data {
        data: String,
    },
    Exit {
        status: u32,
    },
    /// The channel ended. `exitStatus` is null if the server never reported one.
    Closed {
        exit_status: Option<u32>,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The webview reads these exact keys. A rename mismatch shows up as
    /// "undefined" in the UI, so the wire shape is pinned here.
    #[test]
    fn terminal_events_use_camel_case_fields() {
        let closed = serde_json::to_value(TerminalEvent::Closed {
            exit_status: Some(0),
        })
        .expect("json");
        assert_eq!(
            closed,
            serde_json::json!({ "type": "closed", "exitStatus": 0 })
        );
        let unknown =
            serde_json::to_value(TerminalEvent::Closed { exit_status: None }).expect("json");
        assert_eq!(
            unknown,
            serde_json::json!({ "type": "closed", "exitStatus": null })
        );
        let data = serde_json::to_value(TerminalEvent::Data {
            data: "aGk=".into(),
        })
        .expect("json");
        assert_eq!(data, serde_json::json!({ "type": "data", "data": "aGk=" }));
    }

    #[test]
    fn host_view_uses_camel_case_and_never_carries_secrets() {
        let view = HostView {
            id: "x".into(),
            label: "web".into(),
            group_id: None,
            proxy_id: None,
            hostname: "h".into(),
            port: 22,
            username: "u".into(),
            auth_method: "password",
            private_key_path: None,
            connect_timeout_seconds: 15,
            keep_alive_seconds: 30,
            terminal_type: "xterm-256color".into(),
            notes: None,
            credential_configured: true,
            updated_at_ms: 1,
            last_connected_at_ms: None,
            archived: false,
            kind: "ssh",
            wsl_distribution: None,
        };
        let json = serde_json::to_value(&view).expect("json");
        for key in [
            "credentialConfigured",
            "authMethod",
            "keepAliveSeconds",
            "updatedAtMs",
        ] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
        assert!(json.get("password").is_none() && json.get("keyPassphrase").is_none());
    }
}

/// Provider create or update request. The API key is write-only.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInput {
    pub id: Option<String>,
    pub expected_updated_at_ms: Option<i64>,
    pub name: String,
    pub protocol: String,
    pub base_url: String,
    pub model_id: String,
    pub timeout_seconds: u32,
    pub max_output_tokens: u32,
    /// Empty or absent keeps the stored key.
    pub api_key: Option<String>,
    /// True removes the stored key.
    pub clear_api_key: Option<bool>,
    /// Allows plain http to a host other than this machine (for example a LiteLLM
    /// proxy on a private network). The key then travels unencrypted.
    pub allow_insecure_http: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallingView {
    /// `verified` only for the current endpoint and model. `failed` when the last
    /// probe for them did not pass. Otherwise `not_verified`.
    pub status: &'static str,
    pub last_checked_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub protocol: &'static str,
    pub base_url: String,
    pub model_id: String,
    pub timeout_seconds: u32,
    pub max_output_tokens: u32,
    /// Whether an API key is stored. The key itself is never returned.
    pub key_configured: bool,
    pub is_loopback: bool,
    /// Plain http to a non-loopback host was explicitly allowed.
    pub allow_insecure_http: bool,
    pub updated_at_ms: i64,
    pub tool_calling: ToolCallingView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateView {
    pub id: &'static str,
    pub display_name: &'static str,
    pub protocol: &'static str,
    pub default_base_url: &'static str,
    pub note: &'static str,
}

/// Outcome of one provider test. Provider failures are reported here with a
/// code, not thrown, so the UI can explain them.
/// A draft endpoint for the "Check API key" step, before the profile is saved.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelsInput {
    /// When set and `api_key` is empty, the key saved for this provider is used.
    pub id: Option<String>,
    pub protocol: String,
    pub base_url: String,
    pub allow_insecure_http: Option<bool>,
    /// Write-only. Used for this request and dropped; it is not stored here.
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsView {
    pub models: Vec<String>,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTestView {
    /// `connectivity` or `tool_calling`.
    pub kind: &'static str,
    pub ok: bool,
    /// For tool calling: true only when the full round trip passed.
    pub verified: bool,
    pub code: Option<ErrorCode>,
    pub message: String,
    pub latency_ms: u64,
    pub reply_preview: Option<String>,
}
