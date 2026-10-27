//! Agent tasks, command attempts, approval tickets and the audit chain (ADR-0005).
//!
//! Every state change that matters is written in the same transaction as its
//! audit event. A command attempt is recorded before any network I/O: if that
//! write fails, the caller must not run the command.

use rusqlite::{params, OptionalExtension, Row, Transaction, TransactionBehavior};
use selai_audit::{canonical_json, verify_chain, ChainError, ChainEvent, ChainLink};
use selai_domain::{
    ApprovalId, ApprovalState, AuditEventId, ExecStatus, ExecutionId, HostId, Page, PageRequest,
    ProviderId, TaskId, TaskState,
};
use serde_json::Value;

use crate::{Database, StorageError};

pub(crate) fn now_tx(conn: &mut rusqlite::Connection) -> Result<Transaction<'_>, StorageError> {
    Ok(conn.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

fn corrupt(message: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message.to_owned(),
        )),
    )
}

/// Appends one audit event inside `tx`, extending the hash chain.
pub(crate) fn append_audit(
    tx: &Transaction<'_>,
    event_type: &str,
    task: Option<TaskId>,
    execution: Option<ExecutionId>,
    payload: &Value,
    now_ms: i64,
) -> Result<(), StorageError> {
    let (seq, prior): (i64, Option<String>) = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1, (SELECT event_hash FROM audit_events ORDER BY seq DESC LIMIT 1)
           FROM audit_events",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let event = ChainEvent {
        seq,
        event_type: event_type.to_owned(),
        task_id: task.map(|t| t.to_string()),
        execution_id: execution.map(|e| e.to_string()),
        payload: payload.clone(),
        created_at_ms: now_ms,
    };
    let hash = selai_audit::chain_event_hash(prior.as_deref(), &event);
    tx.execute(
        "INSERT INTO audit_events (id, task_id, execution_id, event_type, sanitized_payload_json,
            prior_hash, event_hash, created_at_ms, seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            AuditEventId::new().to_string(),
            event.task_id,
            event.execution_id,
            event_type,
            canonical_json(payload),
            prior,
            hash,
            now_ms,
            seq,
        ],
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    pub id: TaskId,
    pub host: HostId,
    pub provider: Option<ProviderId>,
    pub model_id: String,
    pub limits_json: String,
    /// The operator's request, already redacted. Cut to 500 characters on write.
    pub prompt_preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    pub id: TaskId,
    pub host: HostId,
    pub state: TaskState,
    pub model_id: String,
    pub error_code: Option<String>,
    pub created_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub prompt_preview: Option<String>,
    /// Summed over model turns. `None` when the provider never reported it.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub model_turns: u32,
    /// Command executions recorded for this task.
    pub executions: u32,
}

/// Longest stored request preview, in characters.
pub const PROMPT_PREVIEW_MAX: usize = 500;

pub(crate) const TASK_COLUMNS: &str = "t.id, t.target_host_id, t.state, t.model_id_snapshot, t.error_code,
    t.created_at_ms, t.finished_at_ms, t.prompt_preview, t.input_tokens, t.output_tokens, t.model_turns,
    (SELECT COUNT(*) FROM command_executions c WHERE c.task_id = t.id)";

pub(crate) fn row_to_task(r: &Row<'_>) -> rusqlite::Result<TaskRecord> {
    let id: String = r.get(0)?;
    let host: String = r.get(1)?;
    let state: String = r.get(2)?;
    let input: Option<i64> = r.get(8)?;
    let output: Option<i64> = r.get(9)?;
    Ok(TaskRecord {
        id: id.parse().map_err(|_| corrupt("task id"))?,
        host: host.parse().map_err(|_| corrupt("host id"))?,
        state: TaskState::parse(&state).ok_or_else(|| corrupt("task state"))?,
        model_id: r.get(3)?,
        error_code: r.get(4)?,
        created_at_ms: r.get(5)?,
        finished_at_ms: r.get(6)?,
        prompt_preview: r.get(7)?,
        input_tokens: input.and_then(|v| u64::try_from(v).ok()),
        output_tokens: output.and_then(|v| u64::try_from(v).ok()),
        model_turns: u32::try_from(r.get::<_, i64>(10)?).unwrap_or(u32::MAX),
        executions: u32::try_from(r.get::<_, i64>(11)?).unwrap_or(u32::MAX),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewExecution {
    pub id: ExecutionId,
    pub task: TaskId,
    pub host: HostId,
    pub provider: Option<ProviderId>,
    pub model_id: String,
    pub tool_call_id: Option<String>,
    pub tool_name: String,
    pub command_redacted: String,
    pub command_sha256: String,
    pub policy_decision: String,
    pub now_ms: i64,
}

/// Outcome written when an execution leaves `running` (or is denied or cancelled
/// before running).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionOutcome {
    pub exit_code: Option<i64>,
    pub error_code: Option<String>,
    pub stdout_preview: Option<String>,
    pub stderr_preview: Option<String>,
    pub output_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRecord {
    pub id: ExecutionId,
    pub task: TaskId,
    pub host: HostId,
    pub model_id: String,
    pub tool_name: String,
    pub command_redacted: String,
    pub command_sha256: String,
    pub state: ExecStatus,
    pub policy_decision: Option<String>,
    pub exit_code: Option<i64>,
    pub error_code: Option<String>,
    pub stdout_preview: Option<String>,
    pub stderr_preview: Option<String>,
    pub output_truncated: bool,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewApproval {
    pub id: ApprovalId,
    pub execution: ExecutionId,
    pub policy_version: String,
    pub now_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub id: ApprovalId,
    pub task: TaskId,
    pub execution: ExecutionId,
    pub host: HostId,
    pub command_sha256: String,
    pub state: ApprovalState,
    pub expires_at_ms: i64,
    pub decided_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approve,
    Deny,
}

pub(crate) const EXECUTION_COLUMNS: &str =
    "id, task_id, host_id, model_id_snapshot, tool_name, command_redacted,
    command_sha256, state, policy_decision, exit_code, error_code, stdout_preview, stderr_preview,
    output_truncated, created_at_ms, started_at_ms, finished_at_ms";

pub(crate) fn row_to_execution(row: &Row<'_>) -> rusqlite::Result<ExecutionRecord> {
    let state: String = row.get(7)?;
    let id: String = row.get(0)?;
    let task: String = row.get(1)?;
    let host: String = row.get(2)?;
    let truncated: i64 = row.get(13)?;
    Ok(ExecutionRecord {
        id: id.parse().map_err(|_| corrupt("execution id"))?,
        task: task.parse().map_err(|_| corrupt("task id"))?,
        host: host.parse().map_err(|_| corrupt("host id"))?,
        model_id: row.get(3)?,
        tool_name: row.get(4)?,
        command_redacted: row.get(5)?,
        command_sha256: row.get(6)?,
        state: ExecStatus::parse(&state).ok_or_else(|| corrupt("execution state"))?,
        policy_decision: row.get(8)?,
        exit_code: row.get(9)?,
        error_code: row.get(10)?,
        stdout_preview: row.get(11)?,
        stderr_preview: row.get(12)?,
        output_truncated: truncated == 1,
        created_at_ms: row.get(14)?,
        started_at_ms: row.get(15)?,
        finished_at_ms: row.get(16)?,
    })
}

impl Database {
    pub fn create_agent_task(&mut self, task: &NewTask, now_ms: i64) -> Result<(), StorageError> {
        let tx = now_tx(self.conn_mut())?;
        tx.execute(
            "INSERT INTO agent_tasks (id, target_host_id, provider_profile_id, model_id_snapshot,
                state, limits_json, created_at_ms, prompt_preview)
             VALUES (?1, ?2, ?3, ?4, 'created', ?5, ?6, ?7)",
            params![
                task.id.to_string(),
                task.host.to_string(),
                task.provider.map(|p| p.to_string()),
                task.model_id,
                task.limits_json,
                now_ms,
                task.prompt_preview
                    .as_deref()
                    .map(|p| p.chars().take(PROMPT_PREVIEW_MAX).collect::<String>()),
            ],
        )?;
        append_audit(
            &tx,
            "task_created",
            Some(task.id),
            None,
            &serde_json::json!({"host_id": task.host.to_string(), "model_id": task.model_id}),
            now_ms,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Moves a task to `to` if the transition is legal. The database trigger
    /// repeats the check.
    pub fn transition_task(
        &mut self,
        id: TaskId,
        to: TaskState,
        error_code: Option<&str>,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let tx = now_tx(self.conn_mut())?;
        let current: String = tx
            .query_row(
                "SELECT state FROM agent_tasks WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("task {id}")))?;
        let from = TaskState::parse(&current)
            .ok_or_else(|| StorageError::Constraint("unknown task state".into()))?;
        from.transition(to)
            .map_err(|e| StorageError::Conflict(e.to_string()))?;
        let finished = if to.is_terminal() { Some(now_ms) } else { None };
        tx.execute(
            "UPDATE agent_tasks SET state = ?2, error_code = COALESCE(?3, error_code),
                started_at_ms = COALESCE(started_at_ms, CASE WHEN ?2 = 'running' THEN ?4 END),
                finished_at_ms = COALESCE(?5, finished_at_ms)
              WHERE id = ?1",
            params![id.to_string(), to.as_str(), error_code, now_ms, finished],
        )?;
        append_audit(
            &tx,
            "task_state",
            Some(id),
            None,
            &serde_json::json!({"from": from.as_str(), "to": to.as_str(), "error_code": error_code}),
            now_ms,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_agent_task(&self, id: TaskId) -> Result<Option<TaskRecord>, StorageError> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {TASK_COLUMNS} FROM agent_tasks t WHERE t.id = ?1"),
                params![id.to_string()],
                row_to_task,
            )
            .optional()?)
    }

    /// Adds one model turn's token counts to the task. A count the provider did
    /// not report leaves the stored total as it is (it stays `NULL` if no turn
    /// ever reported it).
    pub fn record_task_usage(
        &mut self,
        id: TaskId,
        input: Option<u64>,
        output: Option<u64>,
    ) -> Result<(), StorageError> {
        let to_sql = |v: Option<u64>| v.map(|n| i64::try_from(n).unwrap_or(i64::MAX));
        let changed = self.conn().execute(
            "UPDATE agent_tasks SET model_turns = model_turns + 1,
                input_tokens = CASE WHEN ?2 IS NULL THEN input_tokens ELSE COALESCE(input_tokens, 0) + ?2 END,
                output_tokens = CASE WHEN ?3 IS NULL THEN output_tokens ELSE COALESCE(output_tokens, 0) + ?3 END
              WHERE id = ?1",
            params![id.to_string(), to_sql(input), to_sql(output)],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(format!("task {id}")));
        }
        Ok(())
    }

    /// Writes an execution row in state `proposed` and its audit event in one
    /// transaction. Must succeed before the command may be sent to the remote host.
    pub fn propose_execution(&mut self, e: &NewExecution) -> Result<(), StorageError> {
        let tx = now_tx(self.conn_mut())?;
        tx.execute(
            "INSERT INTO command_executions (id, task_id, host_id, provider_profile_id, model_id_snapshot,
                tool_call_id, tool_name, command_redacted, command_sha256, state, policy_decision,
                output_truncated, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'proposed', ?10, 0, ?11)",
            params![
                e.id.to_string(),
                e.task.to_string(),
                e.host.to_string(),
                e.provider.map(|p| p.to_string()),
                e.model_id,
                e.tool_call_id,
                e.tool_name,
                e.command_redacted,
                e.command_sha256,
                e.policy_decision,
                e.now_ms,
            ],
        )?;
        append_audit(
            &tx,
            "tool_proposed",
            Some(e.task),
            Some(e.id),
            &serde_json::json!({
                "tool": e.tool_name,
                "host_id": e.host.to_string(),
                "command_sha256": e.command_sha256,
                "policy_decision": e.policy_decision,
            }),
            e.now_ms,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn transition_execution(
        &mut self,
        id: ExecutionId,
        to: ExecStatus,
        outcome: &ExecutionOutcome,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let tx = now_tx(self.conn_mut())?;
        let (task, current): (String, String) = tx
            .query_row(
                "SELECT task_id, state FROM command_executions WHERE id = ?1",
                params![id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("execution {id}")))?;
        let from = ExecStatus::parse(&current)
            .ok_or_else(|| StorageError::Constraint("unknown state".into()))?;
        from.transition(to)
            .map_err(|e| StorageError::Conflict(e.to_string()))?;
        let finished = if to.is_terminal() { Some(now_ms) } else { None };
        tx.execute(
            "UPDATE command_executions SET state = ?2, exit_code = ?3, error_code = ?4,
                stdout_preview = ?5, stderr_preview = ?6, output_truncated = ?7,
                started_at_ms = COALESCE(started_at_ms, CASE WHEN ?2 = 'running' THEN ?8 END),
                finished_at_ms = COALESCE(?9, finished_at_ms)
              WHERE id = ?1",
            params![
                id.to_string(),
                to.as_str(),
                outcome.exit_code,
                outcome.error_code,
                outcome.stdout_preview,
                outcome.stderr_preview,
                i64::from(outcome.output_truncated),
                now_ms,
                finished,
            ],
        )?;
        let task_id: TaskId = task
            .parse()
            .map_err(|_| StorageError::Constraint("task id".into()))?;
        append_audit(
            &tx,
            "execution_state",
            Some(task_id),
            Some(id),
            &serde_json::json!({
                "from": from.as_str(),
                "to": to.as_str(),
                "exit_code": outcome.exit_code,
                "error_code": outcome.error_code,
                "output_truncated": outcome.output_truncated,
            }),
            now_ms,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_execution(&self, id: ExecutionId) -> Result<Option<ExecutionRecord>, StorageError> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {EXECUTION_COLUMNS} FROM command_executions WHERE id = ?1"),
                params![id.to_string()],
                row_to_execution,
            )
            .optional()?)
    }

    /// Attempts for one host, newest first. Keyset pagination on `(created, id)`.
    pub fn host_command_history(
        &self,
        host: HostId,
        page: &PageRequest,
    ) -> Result<Page<ExecutionRecord>, StorageError> {
        let (after_ms, after_id) = match page.cursor.as_deref() {
            None => (i64::MAX, String::from("~")),
            Some(c) => {
                let (ms, id) = c
                    .split_once('|')
                    .ok_or_else(|| StorageError::Constraint("malformed cursor".into()))?;
                let ms: i64 = ms
                    .parse()
                    .map_err(|_| StorageError::Constraint("malformed cursor".into()))?;
                (ms, id.to_owned())
            }
        };
        let limit = i64::from(page.effective_limit());
        let sql = format!(
            "SELECT {EXECUTION_COLUMNS} FROM command_executions
              WHERE host_id = ?1 AND (created_at_ms < ?2 OR (created_at_ms = ?2 AND id < ?3))
              ORDER BY created_at_ms DESC, id DESC LIMIT ?4"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let items = stmt
            .query_map(
                params![host.to_string(), after_ms, after_id, limit],
                row_to_execution,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if items.len() as i64 == limit {
            items
                .last()
                .map(|e| format!("{}|{}", e.created_at_ms, e.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    /// Creates a pending approval ticket and records it in the audit chain. The
    /// execution must already exist, with the same digest (the schema enforces it).
    pub fn create_approval(&mut self, a: &NewApproval) -> Result<(), StorageError> {
        let tx = now_tx(self.conn_mut())?;
        let (task, host, digest): (String, String, String) = tx
            .query_row(
                "SELECT task_id, host_id, command_sha256 FROM command_executions WHERE id = ?1",
                params![a.execution.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("execution {}", a.execution)))?;
        tx.execute(
            "INSERT INTO approval_requests (id, task_id, execution_id, host_id, command_sha256,
                policy_version, state, created_at_ms, expires_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8)",
            params![
                a.id.to_string(),
                task,
                a.execution.to_string(),
                host,
                digest,
                a.policy_version,
                a.now_ms,
                a.expires_at_ms,
            ],
        )?;
        let task_id: TaskId = task
            .parse()
            .map_err(|_| StorageError::Constraint("task id".into()))?;
        append_audit(
            &tx,
            "approval_requested",
            Some(task_id),
            Some(a.execution),
            &serde_json::json!({"approval_id": a.id.to_string(), "expires_at_ms": a.expires_at_ms}),
            a.now_ms,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records a decision. An expired ticket is marked expired and the decision is refused.
    pub fn decide_approval(
        &mut self,
        id: ApprovalId,
        decision: ApprovalDecision,
        now_ms: i64,
    ) -> Result<ApprovalRecord, StorageError> {
        self.decide_approval_inner(id, decision, now_ms, false)
    }

    /// Atomically records the approval and the operator's task-scoped command
    /// authorization in the same audit chain transaction.
    pub fn decide_approval_with_task_scope(
        &mut self,
        id: ApprovalId,
        decision: ApprovalDecision,
        now_ms: i64,
    ) -> Result<ApprovalRecord, StorageError> {
        self.decide_approval_inner(id, decision, now_ms, true)
    }

    fn decide_approval_inner(
        &mut self,
        id: ApprovalId,
        decision: ApprovalDecision,
        now_ms: i64,
        task_scope: bool,
    ) -> Result<ApprovalRecord, StorageError> {
        let tx = now_tx(self.conn_mut())?;
        let record = approval_in(&tx, id)?
            .ok_or_else(|| StorageError::NotFound(format!("approval {id}")))?;
        if record.state != ApprovalState::Pending {
            return Err(StorageError::Conflict(
                "approval has already been decided".into(),
            ));
        }
        let (to, event) = if now_ms > record.expires_at_ms {
            (ApprovalState::Expired, "approval_expired")
        } else {
            match decision {
                ApprovalDecision::Approve => (ApprovalState::Approved, "approval_decided"),
                ApprovalDecision::Deny => (ApprovalState::Denied, "approval_decided"),
            }
        };
        tx.execute(
            "UPDATE approval_requests SET state = ?2, decided_at_ms = ?3 WHERE id = ?1",
            params![id.to_string(), to.as_str(), now_ms],
        )?;
        append_audit(
            &tx,
            event,
            Some(record.task),
            Some(record.execution),
            &serde_json::json!({"approval_id": id.to_string(), "state": to.as_str()}),
            now_ms,
        )?;
        if task_scope && to == ApprovalState::Approved {
            append_audit(
                &tx,
                "task_command_approval_enabled",
                Some(record.task),
                Some(record.execution),
                &serde_json::json!({"scope": "commands_for_task"}),
                now_ms,
            )?;
        }
        tx.commit()?;
        if to == ApprovalState::Expired {
            return Err(StorageError::Conflict(
                "approval expired before it was decided".into(),
            ));
        }
        Ok(ApprovalRecord {
            state: to,
            decided_at_ms: Some(now_ms),
            ..record
        })
    }

    pub fn get_approval(&self, id: ApprovalId) -> Result<Option<ApprovalRecord>, StorageError> {
        approval_in(self.conn(), id)
    }

    pub fn append_audit_event(
        &mut self,
        event_type: &str,
        task: Option<TaskId>,
        execution: Option<ExecutionId>,
        payload: &Value,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let tx = now_tx(self.conn_mut())?;
        append_audit(&tx, event_type, task, execution, payload, now_ms)?;
        tx.commit()?;
        Ok(())
    }

    /// Checks the whole audit chain. Returns the first broken link, if any.
    pub fn verify_audit_chain(&self) -> Result<Result<usize, ChainError>, StorageError> {
        let mut stmt = self.conn().prepare(
            "SELECT seq, task_id, execution_id, event_type, sanitized_payload_json, prior_hash, event_hash, created_at_ms
               FROM audit_events ORDER BY seq",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let payload: String = r.get(4)?;
                let payload: Value =
                    serde_json::from_str(&payload).map_err(|_| corrupt("audit payload"))?;
                Ok((
                    ChainEvent {
                        seq: r.get(0)?,
                        task_id: r.get(1)?,
                        execution_id: r.get(2)?,
                        event_type: r.get(3)?,
                        payload,
                        created_at_ms: r.get(7)?,
                    },
                    ChainLink {
                        prior_hash: r.get(5)?,
                        event_hash: r.get(6)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let count = rows.len();
        Ok(verify_chain(&rows).map(|()| count))
    }
}

fn approval_in(
    conn: &rusqlite::Connection,
    id: ApprovalId,
) -> Result<Option<ApprovalRecord>, StorageError> {
    Ok(conn
        .query_row(
            "SELECT id, task_id, execution_id, host_id, command_sha256, state, expires_at_ms, decided_at_ms
               FROM approval_requests WHERE id = ?1",
            params![id.to_string()],
            |r| {
                let id: String = r.get(0)?;
                let task: String = r.get(1)?;
                let exec: String = r.get(2)?;
                let host: String = r.get(3)?;
                let state: String = r.get(5)?;
                Ok(ApprovalRecord {
                    id: id.parse().map_err(|_| corrupt("approval id"))?,
                    task: task.parse().map_err(|_| corrupt("task id"))?,
                    execution: exec.parse().map_err(|_| corrupt("execution id"))?,
                    host: host.parse().map_err(|_| corrupt("host id"))?,
                    command_sha256: r.get(4)?,
                    state: ApprovalState::parse(&state).ok_or_else(|| corrupt("approval state"))?,
                    expires_at_ms: r.get(6)?,
                    decided_at_ms: r.get(7)?,
                })
            },
        )
        .optional()?)
}

impl Database {
    /// Marks a pending ticket expired, but only if its deadline has passed.
    /// Before the deadline this is a no-op, so a ticket can never be approved by accident.
    pub fn expire_approval(&mut self, id: ApprovalId, now_ms: i64) -> Result<bool, StorageError> {
        let tx = now_tx(self.conn_mut())?;
        let Some(record) = approval_in(&tx, id)? else {
            return Err(StorageError::NotFound(format!("approval {id}")));
        };
        if record.state != ApprovalState::Pending || now_ms <= record.expires_at_ms {
            return Ok(false);
        }
        tx.execute(
            "UPDATE approval_requests SET state = 'expired', decided_at_ms = ?2 WHERE id = ?1",
            params![id.to_string(), now_ms],
        )?;
        append_audit(
            &tx,
            "approval_expired",
            Some(record.task),
            Some(record.execution),
            &serde_json::json!({"approval_id": id.to_string()}),
            now_ms,
        )?;
        tx.commit()?;
        Ok(true)
    }
}
