//! Per-host AI history: filtered views, task timelines, sanitized export,
//! output purge and the few preferences the AI tab remembers (S-040, S-042).

use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use selai_audit::redact;
use selai_domain::{ErrorCode, ExecStatus, HostId, Page, PageRequest, ProviderId, TaskId};
use selai_storage::{HistoryFilter, PurgeScope, TaskRecord};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    agent_service::{execution_view, ExecutionView},
    dto::CommandError,
    service::Services,
};

/// Days of stored command output kept when the user has not chosen otherwise.
pub const DEFAULT_RETENTION_DAYS: u32 = 30;
const MAX_RETENTION_DAYS: u32 = 3650;
/// Rows written to one export. The file says when it was cut short.
pub const EXPORT_ROW_LIMIT: usize = 1000;
const RETENTION_KEY: &str = "ai.output_retention_days";
const LAST_PROVIDER_KEY: &str = "ai.last_provider_id";
const PINNED_HOST_KEY: &str = "ai.pinned_host_id";
const TASK_MAX_STEPS_KEY: &str = "ai.task_max_steps";
const TASK_MAX_MINUTES_KEY: &str = "ai.task_max_minutes";
pub const DEFAULT_TASK_MAX_STEPS: u32 = 30;
pub const DEFAULT_TASK_MAX_MINUTES: u32 = 30;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryFilterInput {
    /// Execution states such as `completed` or `denied`. Empty means all.
    #[serde(default)]
    pub states: Vec<String>,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
}

impl HistoryFilterInput {
    fn parse(&self) -> Result<HistoryFilter, CommandError> {
        let states = self
            .states
            .iter()
            .map(|s| {
                ExecStatus::parse(s).ok_or_else(|| {
                    CommandError::new(ErrorCode::InvalidInput, format!("unknown state: {s}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let (Some(since), Some(until)) = (self.since_ms, self.until_ms) {
            if since > until {
                return Err(CommandError::new(
                    ErrorCode::InvalidInput,
                    "from must not be after to",
                ));
            }
        }
        Ok(HistoryFilter {
            states,
            since_ms: self.since_ms,
            until_ms: self.until_ms,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub id: String,
    pub host_id: String,
    pub state: &'static str,
    pub model_id: String,
    pub error_code: Option<String>,
    pub created_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    /// The operator's request, redacted before it was stored.
    pub prompt_preview: Option<String>,
    /// `None` when the provider never reported the count.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub model_turns: u32,
    pub executions: u32,
}

/// Totals for one host's AI activity.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostUsageView {
    pub tasks: u32,
    pub executions: u32,
    pub failed_executions: u32,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub last_activity_ms: Option<i64>,
}

fn task_view(t: TaskRecord) -> TaskView {
    TaskView {
        id: t.id.to_string(),
        host_id: t.host.to_string(),
        state: task_state_text(t.state),
        model_id: t.model_id,
        error_code: t.error_code,
        created_at_ms: t.created_at_ms,
        finished_at_ms: t.finished_at_ms,
        prompt_preview: t.prompt_preview,
        input_tokens: t.input_tokens,
        output_tokens: t.output_tokens,
        model_turns: t.model_turns,
        executions: t.executions,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub path: String,
    pub executions: usize,
    /// True when the host has more rows than one export holds.
    pub truncated_rows: bool,
    /// Secret-like text replaced during this export, beyond what was stored.
    pub extra_redactions: usize,
    pub disclosure: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiPreferences {
    pub last_provider_id: Option<String>,
    pub output_retention_days: u32,
    /// The server the chat is pinned to. Messages without an `@` mention run there.
    pub pinned_host_id: Option<String>,
    pub task_max_steps: u32,
    pub task_max_minutes: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiPreferencesInput {
    pub last_provider_id: Option<String>,
    pub output_retention_days: Option<u32>,
    /// `""` unpins the chat; a host id pins it to that host.
    pub pinned_host_id: Option<String>,
    pub task_max_steps: Option<u32>,
    pub task_max_minutes: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeResult {
    pub executions: usize,
}

pub const EXPORT_DISCLOSURE: &str = "Output previews are bounded when stored. The export \
    redacts known secret patterns again, but it cannot find every sensitive value. Read it \
    before sharing. Rows beyond the limit are not included.";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

fn parse_host(id: &str) -> Result<HostId, CommandError> {
    id.parse()
        .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid host id"))
}

fn storage(e: &selai_storage::StorageError) -> CommandError {
    use selai_domain::SelaiError;
    CommandError::new(e.code(), e.to_string())
}

fn task_state_text(state: selai_domain::TaskState) -> &'static str {
    state.as_str()
}

/// Days of output to keep. Anything outside 1..=3650 is refused by the caller.
fn retention_from(value: Option<Value>) -> u32 {
    value
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| (1..=MAX_RETENTION_DAYS).contains(n))
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

impl Services {
    /// Removes stored output older than the retention window. Metadata stays and
    /// the action is audited. Runs at activation and periodically while unlocked.
    pub(crate) fn apply_retention(&self, now: i64) -> Result<usize, CommandError> {
        let mut db = self.lock_db_pub()?;
        let days = retention_from(db.get_setting(RETENTION_KEY).map_err(|e| storage(&e))?);
        let window_ms = i64::from(days) * 24 * 60 * 60 * 1000;
        db.purge_output(PurgeScope::FinishedBefore(now - window_ms), now)
            .map_err(|e| storage(&e))
    }

    pub fn history_query(
        &self,
        host_id: &str,
        filter: &HistoryFilterInput,
        cursor: Option<String>,
    ) -> Result<Page<ExecutionView>, CommandError> {
        let host = parse_host(host_id)?;
        let filter = filter.parse()?;
        let db = self.lock_db_pub()?;
        let page = db
            .host_history_filtered(host, &filter, &PageRequest { cursor, limit: 50 })
            .map_err(|e| storage(&e))?;
        Ok(Page {
            items: page.items.into_iter().map(execution_view).collect(),
            next_cursor: page.next_cursor,
        })
    }

    pub fn history_tasks(
        &self,
        host_id: &str,
        cursor: Option<String>,
    ) -> Result<Page<TaskView>, CommandError> {
        let host = parse_host(host_id)?;
        let db = self.lock_db_pub()?;
        let page = db
            .host_tasks(host, &PageRequest { cursor, limit: 50 })
            .map_err(|e| storage(&e))?;
        Ok(Page {
            items: page.items.into_iter().map(task_view).collect(),
            next_cursor: page.next_cursor,
        })
    }

    pub fn history_summary(&self, host_id: &str) -> Result<HostUsageView, CommandError> {
        let host = parse_host(host_id)?;
        let db = self.lock_db_pub()?;
        let s = db.host_usage_summary(host).map_err(|e| storage(&e))?;
        Ok(HostUsageView {
            tasks: s.tasks,
            executions: s.executions,
            failed_executions: s.failed_executions,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            last_activity_ms: s.last_activity_ms,
        })
    }

    pub fn task_timeline(&self, task_id: &str) -> Result<Vec<ExecutionView>, CommandError> {
        let task: TaskId = task_id
            .parse()
            .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid task id"))?;
        let db = self.lock_db_pub()?;
        Ok(db
            .task_timeline(task)
            .map_err(|e| storage(&e))?
            .into_iter()
            .map(execution_view)
            .collect())
    }

    /// Writes the filtered host history to a JSON file in the app's export folder.
    /// Previews are redacted once more, the file says what was cut or replaced,
    /// and the export is audited by row count only.
    pub fn history_export(
        &self,
        host_id: &str,
        filter: &HistoryFilterInput,
    ) -> Result<ExportResult, CommandError> {
        let host = parse_host(host_id)?;
        let filter = filter.parse()?;
        let dir = self.export_dir.clone().ok_or_else(|| {
            CommandError::new(ErrorCode::StorageIo, "exports need a database file on disk")
        })?;

        let db = self.lock_db_pub()?;
        let record = db
            .get_host(host)
            .map_err(|e| storage(&e))?
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "host not found"))?;

        let mut rows = Vec::new();
        let mut cursor = None;
        let mut truncated_rows = false;
        loop {
            let page = db
                .host_history_filtered(
                    host,
                    &filter,
                    &PageRequest {
                        cursor: cursor.clone(),
                        limit: 200,
                    },
                )
                .map_err(|e| storage(&e))?;
            rows.extend(page.items);
            if rows.len() > EXPORT_ROW_LIMIT {
                rows.truncate(EXPORT_ROW_LIMIT);
                truncated_rows = true;
                break;
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        let mut extra_redactions = 0usize;
        let mut clean = |text: Option<String>| -> Option<String> {
            text.map(|t| {
                let r = redact(&t, &[]);
                extra_redactions += r.replacements;
                r.text
            })
        };
        let entries: Vec<Value> = rows
            .iter()
            .map(|e| {
                json!({
                    "id": e.id.to_string(),
                    "taskId": e.task.to_string(),
                    "tool": e.tool_name,
                    "modelId": e.model_id,
                    "command": e.command_redacted,
                    "commandSha256": e.command_sha256,
                    "state": e.state.as_str(),
                    "policyDecision": e.policy_decision,
                    "exitCode": e.exit_code,
                    "errorCode": e.error_code,
                    "stdoutPreview": clean(e.stdout_preview.clone()),
                    "stderrPreview": clean(e.stderr_preview.clone()),
                    "outputTruncated": e.output_truncated,
                    "createdAtMs": e.created_at_ms,
                    "startedAtMs": e.started_at_ms,
                    "finishedAtMs": e.finished_at_ms,
                })
            })
            .collect();

        let now = now_ms();
        let document = json!({
            "format": "selai-history-v1",
            "exportedAtMs": now,
            "host": { "id": record.id.to_string(), "label": record.label, "archived": record.archived_at_ms.is_some() },
            "rowCount": entries.len(),
            "rowLimit": EXPORT_ROW_LIMIT,
            "truncatedRows": truncated_rows,
            "disclosure": EXPORT_DISCLOSURE,
            "executions": entries,
        });

        fs::create_dir_all(&dir)
            .map_err(|e| CommandError::new(ErrorCode::StorageIo, e.to_string()))?;
        let extension = if self.document_sealer.is_some() {
            "selai-history"
        } else {
            "json"
        };
        let file = dir.join(format!("selai-history-{}-{now}.{extension}", record.id));
        let text = serde_json::to_string_pretty(&document)
            .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?;
        drop(db);
        let text = zeroize::Zeroizing::new(text);
        let bytes = match &self.document_sealer {
            Some(seal) => seal(text.as_bytes())?,
            None => text.as_bytes().to_vec(),
        };
        fs::write(&file, bytes)
            .map_err(|e| CommandError::new(ErrorCode::StorageIo, e.to_string()))?;

        self.lock_db_pub()?.append_audit_event(
            "history_exported",
            None,
            None,
            &json!({ "host_id": record.id.to_string(), "executions": rows.len(), "truncated_rows": truncated_rows }),
            now,
        )
        .map_err(|e| storage(&e))?;

        Ok(ExportResult {
            path: file.display().to_string(),
            executions: rows.len(),
            truncated_rows,
            extra_redactions,
            disclosure: EXPORT_DISCLOSURE,
        })
    }

    /// Clears stored output for one host, keeping its command history.
    pub fn history_purge_output(&self, host_id: &str) -> Result<PurgeResult, CommandError> {
        let host = parse_host(host_id)?;
        let mut db = self.lock_db_pub()?;
        let executions = db
            .purge_output(PurgeScope::Host(host), now_ms())
            .map_err(|e| storage(&e))?;
        Ok(PurgeResult { executions })
    }

    pub fn preferences_get(&self) -> Result<AiPreferences, CommandError> {
        let db = self.lock_db_pub()?;
        let last = db
            .get_setting(LAST_PROVIDER_KEY)
            .map_err(|e| storage(&e))?
            .and_then(|v| v.as_str().map(str::to_owned));
        let days = retention_from(db.get_setting(RETENTION_KEY).map_err(|e| storage(&e))?);
        let pinned = db
            .get_setting(PINNED_HOST_KEY)
            .map_err(|e| storage(&e))?
            .and_then(|v| v.as_str().map(str::to_owned))
            .filter(|id| !id.is_empty());
        let task_max_steps = preference_u32(
            db.get_setting(TASK_MAX_STEPS_KEY)
                .map_err(|e| storage(&e))?,
            DEFAULT_TASK_MAX_STEPS,
            1..=300,
        );
        let task_max_minutes = preference_u32(
            db.get_setting(TASK_MAX_MINUTES_KEY)
                .map_err(|e| storage(&e))?,
            DEFAULT_TASK_MAX_MINUTES,
            1..=300,
        );
        Ok(AiPreferences {
            last_provider_id: last,
            output_retention_days: days,
            pinned_host_id: pinned,
            task_max_steps,
            task_max_minutes,
        })
    }

    /// Stores the AI tab's remembered choices. The provider must exist; the
    /// retention window must be 1 to 3650 days.
    pub fn preferences_set(
        &self,
        input: AiPreferencesInput,
    ) -> Result<AiPreferences, CommandError> {
        let mut db = self.lock_db_pub()?;
        let now = now_ms();
        if let Some(id) = &input.last_provider_id {
            let provider: ProviderId = id
                .parse()
                .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid provider id"))?;
            if db
                .get_provider(provider)
                .map_err(|e| storage(&e))?
                .is_none()
            {
                return Err(CommandError::new(ErrorCode::NotFound, "provider not found"));
            }
            db.set_setting(LAST_PROVIDER_KEY, &json!(id), now)
                .map_err(|e| storage(&e))?;
        }
        if let Some(pin) = &input.pinned_host_id {
            if pin.is_empty() {
                db.set_setting(PINNED_HOST_KEY, &json!(""), now)
                    .map_err(|e| storage(&e))?;
            } else {
                let host: HostId = pin
                    .parse()
                    .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid host id"))?;
                let record = db
                    .get_host(host)
                    .map_err(|e| storage(&e))?
                    .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "host not found"))?;
                if record.archived_at_ms.is_some() {
                    return Err(CommandError::new(
                        ErrorCode::InvalidInput,
                        "an archived host cannot be pinned",
                    ));
                }
                db.set_setting(PINNED_HOST_KEY, &json!(pin), now)
                    .map_err(|e| storage(&e))?;
            }
        }
        if let Some(days) = input.output_retention_days {
            if !(1..=MAX_RETENTION_DAYS).contains(&days) {
                return Err(CommandError::new(
                    ErrorCode::InvalidInput,
                    format!("retention must be 1 to {MAX_RETENTION_DAYS} days"),
                ));
            }
            db.set_setting(RETENTION_KEY, &json!(days), now)
                .map_err(|e| storage(&e))?;
        }
        if let Some(steps) = input.task_max_steps {
            if !(1..=300).contains(&steps) {
                return Err(CommandError::new(
                    ErrorCode::InvalidInput,
                    "task step limit must be 1 to 300",
                ));
            }
            db.set_setting(TASK_MAX_STEPS_KEY, &json!(steps), now)
                .map_err(|e| storage(&e))?;
        }
        if let Some(minutes) = input.task_max_minutes {
            if !(1..=300).contains(&minutes) {
                return Err(CommandError::new(
                    ErrorCode::InvalidInput,
                    "task time limit must be 1 to 300 minutes",
                ));
            }
            db.set_setting(TASK_MAX_MINUTES_KEY, &json!(minutes), now)
                .map_err(|e| storage(&e))?;
        }
        drop(db);
        self.preferences_get()
    }
}

fn preference_u32(value: Option<Value>, default: u32, range: std::ops::RangeInclusive<u32>) -> u32 {
    value
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| range.contains(value))
        .unwrap_or(default)
}

/// The export folder sits next to the database file. Used by `Services::open`.
pub fn export_dir_for(db_path: &std::path::Path) -> Option<PathBuf> {
    db_path.parent().map(|p| p.join("exports"))
}
