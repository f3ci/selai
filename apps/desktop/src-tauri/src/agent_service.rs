//! Agent tasks in the app: starting a task on one host, approvals, cancellation,
//! per-host history and audit verification (S-030, S-033, S-040).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use selai_agent::{
    run_task, ApprovalWaiter, DbStore, EventSink, Limits, RemoteExecutor, TaskEvent, TaskSpec,
    WaitOutcome,
};
use selai_domain::AuthMethod;
use selai_domain::{
    ApprovalId, ErrorCode, HostId, ModelId, Page, PageRequest, ProviderId, SelaiError, TaskId,
};
use selai_secrets::{SecretKey, SecretValue, SshSecretKind};
use selai_ssh::{connect_via, ConnectOptions, Endpoint, ExecOutput, Session, SshError, SudoStatus};
use selai_storage::{ApprovalDecision, Database};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::{
    chat_service::{prior_messages, ChatMessageInput},
    dto::CommandError,
    local_exec::LocalExecutor,
    provider_service::{settings_from_record, tool_calling_view},
    service::Services,
};

const PROMPT_MAX_BYTES: usize = 4000;

/// Waiters and early decisions share one lock, so a decision either reaches a
/// registered waiter or is kept for the next `wait`, never lost in between.
#[derive(Default)]
struct Approvals {
    waiters: HashMap<ApprovalId, oneshot::Sender<ApprovalDecision>>,
    /// Decisions that arrived before their task started waiting. The runtime
    /// emits the approval event before it calls `wait`, so a fast click can land
    /// first.
    early: HashMap<ApprovalId, ApprovalDecision>,
}

/// Live tasks and approvals waiting for a decision.
#[derive(Default)]
pub struct AgentRegistry {
    pub(crate) drain: CancellationToken,
    tokens: Mutex<HashMap<TaskId, CancellationToken>>,
    approvals: Mutex<Approvals>,
    /// Sudo passwords given with an approval, held in memory until the task
    /// takes them for that one command.
    sudo_passwords: Mutex<HashMap<ApprovalId, SecretValue>>,
    /// Explicit root-command grants last only for one running task.
    root_approved_tasks: Mutex<HashMap<TaskId, selai_domain::ExecutionId>>,
    /// When needed, the password attached to that grant stays in memory only
    /// until the task ends or the vault locks.
    task_sudo_passwords: Mutex<HashMap<TaskId, SecretValue>>,
}

impl AgentRegistry {
    pub(crate) fn register_task(&self, task: TaskId) -> CancellationToken {
        let token = CancellationToken::new();
        if self.drain.is_cancelled() {
            token.cancel();
        }
        if let Ok(mut map) = self.tokens.lock() {
            map.insert(task, token.clone());
        }
        token
    }

    pub(crate) fn finish_task(&self, task: TaskId) {
        if let Ok(mut map) = self.tokens.lock() {
            map.remove(&task);
        }
        if let Ok(mut tasks) = self.root_approved_tasks.lock() {
            tasks.remove(&task);
        }
        if let Ok(mut passwords) = self.task_sudo_passwords.lock() {
            passwords.remove(&task);
        }
    }

    fn cancel_task(&self, task: TaskId) -> bool {
        self.tokens
            .lock()
            .ok()
            .and_then(|map| map.get(&task).cloned())
            .map(|t| t.cancel())
            .is_some()
    }

    pub(crate) fn active_count(&self) -> usize {
        self.tokens
            .lock()
            .map(|tokens| tokens.len())
            .unwrap_or(usize::MAX)
    }

    pub(crate) fn cancel_all(&self) {
        self.drain.cancel();
        if let Ok(tokens) = self.tokens.lock() {
            for token in tokens.values() {
                token.cancel();
            }
        }
    }

    pub(crate) fn clear_sensitive(&self) {
        if let Ok(mut passwords) = self.sudo_passwords.lock() {
            passwords.clear();
        }
        if let Ok(mut passwords) = self.task_sudo_passwords.lock() {
            passwords.clear();
        }
        if let Ok(mut tasks) = self.root_approved_tasks.lock() {
            tasks.clear();
        }
        if let Ok(mut approvals) = self.approvals.lock() {
            approvals.early.clear();
            approvals.waiters.clear();
        }
    }

    fn hold_sudo_password(&self, approval: ApprovalId, password: SecretValue) {
        if let Ok(mut map) = self.sudo_passwords.lock() {
            map.insert(approval, password);
        }
    }

    fn take_sudo_password(&self, approval: ApprovalId) -> Option<SecretValue> {
        self.sudo_passwords.lock().ok()?.remove(&approval)
    }

    fn copy_sudo_password(&self, approval: ApprovalId) -> Option<SecretValue> {
        let passwords = self.sudo_passwords.lock().ok()?;
        passwords
            .get(&approval)
            .map(|password| SecretValue::new(password.expose_for_transport().to_vec()))
    }

    fn approve_root_for_task(
        &self,
        task: TaskId,
        first_execution: selai_domain::ExecutionId,
        password: Option<SecretValue>,
    ) {
        if let Ok(mut tasks) = self.root_approved_tasks.lock() {
            tasks.insert(task, first_execution);
        }
        if let Some(password) = password {
            if let Ok(mut passwords) = self.task_sudo_passwords.lock() {
                passwords.insert(task, password);
            }
        }
    }

    fn deliver(&self, approval: ApprovalId, decision: ApprovalDecision) {
        let Ok(mut state) = self.approvals.lock() else {
            return;
        };
        match state.waiters.remove(&approval) {
            Some(tx) => {
                let _ = tx.send(decision);
            }
            None => {
                state.early.insert(approval, decision);
            }
        }
    }
}

/// Waits for an operator decision delivered through the registry. The database
/// decides validity; this only wakes the task.
struct RegistryWaiter {
    registry: Arc<AgentRegistry>,
}

impl ApprovalWaiter for RegistryWaiter {
    async fn wait(
        &self,
        approval: ApprovalId,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> WaitOutcome {
        let rx = {
            let Ok(mut state) = self.registry.approvals.lock() else {
                return WaitOutcome::TimedOut;
            };
            if let Some(decision) = state.early.remove(&approval) {
                return WaitOutcome::Decided(decision);
            }
            let (tx, rx) = oneshot::channel();
            state.waiters.insert(approval, tx);
            rx
        };
        let outcome = tokio::select! {
            () = self.registry.drain.cancelled() => WaitOutcome::Cancelled,
            () = cancel.cancelled() => WaitOutcome::Cancelled,
            r = tokio::time::timeout(timeout, rx) => match r {
                Ok(Ok(decision)) => WaitOutcome::Decided(decision),
                _ => WaitOutcome::TimedOut,
            },
        };
        if let Ok(mut state) = self.registry.approvals.lock() {
            state.waiters.remove(&approval);
        }
        outcome
    }

    fn take_sudo_password(&self, approval: ApprovalId) -> Option<SecretValue> {
        self.registry.take_sudo_password(approval)
    }

    fn task_root_approved(&self, task: TaskId, execution: selai_domain::ExecutionId) -> bool {
        self.registry
            .root_approved_tasks
            .lock()
            .map(|tasks| tasks.get(&task).is_some_and(|first| *first != execution))
            .unwrap_or(false)
    }

    fn task_sudo_password(&self, task: TaskId) -> Option<SecretValue> {
        let passwords = self.registry.task_sudo_passwords.lock().ok()?;
        let password = passwords.get(&task)?;
        Some(SecretValue::new(password.expose_for_transport().to_vec()))
    }
}

/// Runs commands on the task's own SSH session. Holds no credential.
struct SessionExecutor {
    session: Arc<Session>,
}

impl RemoteExecutor for SessionExecutor {
    fn run(
        &self,
        command: &str,
        timeout: Duration,
        max_bytes_per_stream: usize,
        cancel: &CancellationToken,
    ) -> impl std::future::Future<Output = Result<ExecOutput, SshError>> + Send {
        let session = Arc::clone(&self.session);
        let command = command.to_owned();
        let cancel = cancel.clone();
        async move {
            session
                .exec(&command, timeout, max_bytes_per_stream, &cancel)
                .await
        }
    }

    fn sudo_status(
        &self,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<SudoStatus, SshError>> + Send {
        let session = Arc::clone(&self.session);
        async move { session.sudo_status(timeout).await }
    }

    fn run_as_root(
        &self,
        command: &str,
        password: Option<&SecretValue>,
        timeout: Duration,
        max_bytes_per_stream: usize,
        cancel: &CancellationToken,
    ) -> impl std::future::Future<Output = Result<ExecOutput, SshError>> + Send {
        let session = Arc::clone(&self.session);
        let command = command.to_owned();
        let password = password.map(|p| SecretValue::new(p.expose_for_transport().to_vec()));
        let cancel = cancel.clone();
        async move {
            session
                .exec_as_root(
                    &command,
                    password.as_ref(),
                    timeout,
                    max_bytes_per_stream,
                    &cancel,
                )
                .await
        }
    }
}

/// How the operator answers sudo's password question for one approval.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SudoAnswerInput {
    /// `typed`, `saved` or `login`.
    pub source: String,
    /// Write-only. Only with `typed`.
    pub password: Option<String>,
    /// With `typed`: also save it in the OS credential store for this host.
    pub remember: Option<bool>,
}

/// Which sudo password sources exist for an approval's host. Never the values.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SudoOptionsView {
    pub host_label: String,
    pub saved_password: bool,
    /// The host signs in with a saved SSH password, which may also be its sudo password.
    pub login_password: bool,
}

const SUDO_PASSWORD_MAX_BYTES: usize = 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TaskEventView {
    State {
        state: String,
        error_code: Option<String>,
    },
    TextDelta {
        text: String,
    },
    ToolProposed {
        execution_id: String,
        tool: String,
        command: String,
        host_label: String,
    },
    ApprovalRequired {
        approval_id: String,
        execution_id: String,
        command: String,
        expires_at_ms: i64,
        /// For a root command: `no_password` or `password_required`.
        sudo: Option<&'static str>,
    },
    ToolFinished {
        execution_id: String,
        state: String,
        exit_code: Option<i64>,
        error_code: Option<String>,
    },
    Usage {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    },
    Completed {
        text: String,
    },
    Failed {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEventView {
    pub task_id: String,
    #[serde(flatten)]
    pub event: TaskEventView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionView {
    pub id: String,
    pub task_id: String,
    pub host_id: String,
    pub model_id: String,
    pub tool: String,
    pub command_redacted: String,
    pub command_sha256: String,
    pub state: &'static str,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditVerifyView {
    pub ok: bool,
    pub events_checked: usize,
    pub broken_at_seq: Option<i64>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

fn event_view(event: TaskEvent) -> TaskEventView {
    match event {
        TaskEvent::State { state, error_code } => TaskEventView::State { state, error_code },
        TaskEvent::TextDelta { text } => TaskEventView::TextDelta { text },
        TaskEvent::ToolProposed {
            execution,
            tool,
            command,
            host_label,
        } => TaskEventView::ToolProposed {
            execution_id: execution.to_string(),
            tool,
            command,
            host_label,
        },
        TaskEvent::ApprovalRequired {
            approval,
            execution,
            command,
            expires_at_ms,
            sudo,
        } => TaskEventView::ApprovalRequired {
            approval_id: approval.to_string(),
            execution_id: execution.to_string(),
            command,
            expires_at_ms,
            sudo: sudo.map(SudoStatus::as_str),
        },
        TaskEvent::ToolFinished {
            execution,
            state,
            exit_code,
            error_code,
        } => TaskEventView::ToolFinished {
            execution_id: execution.to_string(),
            state,
            exit_code,
            error_code,
        },
        TaskEvent::Usage {
            input_tokens,
            output_tokens,
        } => TaskEventView::Usage {
            input_tokens,
            output_tokens,
        },
        TaskEvent::Completed { text } => TaskEventView::Completed { text },
        TaskEvent::Failed { code, message } => TaskEventView::Failed { code, message },
    }
}

pub(crate) fn execution_view(e: selai_storage::ExecutionRecord) -> ExecutionView {
    ExecutionView {
        id: e.id.to_string(),
        task_id: e.task.to_string(),
        host_id: e.host.to_string(),
        model_id: e.model_id,
        tool: e.tool_name,
        command_redacted: e.command_redacted,
        command_sha256: e.command_sha256,
        state: e.state.as_str(),
        policy_decision: e.policy_decision,
        exit_code: e.exit_code,
        error_code: e.error_code,
        stdout_preview: e.stdout_preview,
        stderr_preview: e.stderr_preview,
        output_truncated: e.output_truncated,
        created_at_ms: e.created_at_ms,
        started_at_ms: e.started_at_ms,
        finished_at_ms: e.finished_at_ms,
    }
}

fn storage(e: &selai_storage::StorageError) -> CommandError {
    CommandError::new(e.code(), e.to_string())
}

impl Services {
    /// Starts one task on one host. The host is fixed here, from the mention chip;
    /// the model cannot choose it. Refuses if the provider's tool calling has not
    /// been verified for its current endpoint and model.
    pub async fn agent_start(
        &self,
        host_id: &str,
        provider_id: &str,
        prompt: &str,
        history: &[ChatMessageInput],
        on_event: Arc<dyn Fn(AgentEventView) + Send + Sync>,
    ) -> Result<String, CommandError> {
        self.agent_start_with_model(host_id, provider_id, None, prompt, history, on_event)
            .await
    }

    pub async fn agent_start_with_model(
        &self,
        host_id: &str,
        provider_id: &str,
        model_id: Option<&str>,
        prompt: &str,
        history: &[ChatMessageInput],
        on_event: Arc<dyn Fn(AgentEventView) + Send + Sync>,
    ) -> Result<String, CommandError> {
        let history = prior_messages(history)?;
        let host: HostId = host_id
            .parse()
            .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid host id"))?;
        let provider: ProviderId = provider_id
            .parse()
            .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid provider id"))?;
        let prompt = prompt.trim();
        if prompt.is_empty() || prompt.len() > PROMPT_MAX_BYTES {
            return Err(CommandError::new(
                ErrorCode::InvalidInput,
                "the request must be 1 to 4000 bytes",
            ));
        }

        let (host_record, pins) = self.active_host_and_pins(host)?;
        let rec = self
            .lock_db_pub()?
            .get_provider(provider)
            .map_err(|e| storage(&e))?
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "provider not found"))?;
        let selected_model = model_id.unwrap_or(&rec.model_id);
        if selected_model != rec.model_id || tool_calling_view(&rec).status != "verified" {
            return Err(CommandError::new(
                ErrorCode::ProviderToolUnsupported,
                "run the tool calling test for this provider before starting a task",
            ));
        }
        let mut settings = settings_from_record(&rec)?;
        settings.model_id = ModelId::parse(selected_model)
            .map_err(|e| CommandError::new(e.code(), e.to_string()))?;
        let api_key = self
            .secrets
            .get(&SecretKey::provider_api_key(provider))
            .map_err(|e| CommandError::new(e.code(), e.to_string()))?;

        let task = TaskId::new();
        let spec = TaskSpec {
            drain: self.agent.drain.clone(),
            task,
            host,
            host_label: host_record.label.clone(),
            host_context: if host_record.is_local {
                match &host_record.wsl_distribution {
                    Some(distribution) => format!("{} (Linux in WSL distribution {distribution} on this computer; commands run in sh)", host_record.label),
                    None => format!("{} (this computer, {} local shell)", host_record.label, std::env::consts::OS),
                }
            } else {
                format!(
                    "{} ({}@{}:{})",
                    host_record.label, host_record.username, host_record.hostname, host_record.port
                )
            },
            provider: Some(provider),
            settings,
            api_key,
            history,
            prompt: prompt.to_owned(),
            limits: {
                let prefs = self.preferences_get()?;
                Limits {
                    max_steps: prefs.task_max_steps,
                    max_wall: Duration::from_secs(u64::from(prefs.task_max_minutes) * 60),
                    ..Limits::default()
                }
            },
        };
        let sink: EventSink = Arc::new(move |event| on_event(wire_event(task, event)));
        let store = DbStore::new(Arc::clone(&self.db));
        let llm = Arc::clone(&self.llm);
        let registry = Arc::clone(&self.agent);

        if host_record.is_local {
            // This computer: no SSH session, no host key. Same policy, approvals
            // and audit as any host.
            let executor = LocalExecutor::new(host_record.wsl_distribution)
                .map_err(|error| CommandError::new(error.code(), error.to_string()))?;
            let token = self.agent.register_task(task);
            tokio::spawn(async move {
                let waiter = RegistryWaiter {
                    registry: Arc::clone(&registry),
                };
                run_task(spec, &store, &llm, &executor, &waiter, sink, token).await;
                registry.finish_task(task);
            });
            return Ok(task.to_string());
        }

        let auth = self.load_auth(host, &host_record)?;
        let session = connect_via(
            ConnectOptions {
                endpoint: Endpoint::new(host_record.hostname.clone(), host_record.port),
                username: host_record.username.clone(),
                auth,
                pinned: pins,
                connect_timeout: Duration::from_secs(u64::from(
                    host_record.connect_timeout_seconds,
                )),
                keepalive: match host_record.keepalive_seconds {
                    0 => None,
                    s => Some(Duration::from_secs(u64::from(s))),
                },
            },
            self.proxy_for_host(host)?,
        )
        .await
        .map_err(|e| {
            // Same review-and-trust path as the terminal: keep the presented key.
            self.remember_observed_key(host, &e);
            crate::service::ssh_error(&e)
        })?;

        let token = self.agent.register_task(task);
        let session = Arc::new(session);
        tokio::spawn(async move {
            let executor = SessionExecutor {
                session: Arc::clone(&session),
            };
            let waiter = RegistryWaiter {
                registry: Arc::clone(&registry),
            };
            run_task(spec, &store, &llm, &executor, &waiter, sink, token).await;
            // The executor holds a clone of the session; release it first so the
            // session is the last owner and can be closed cleanly.
            drop(executor);
            registry.finish_task(task);
            if let Ok(s) = Arc::try_unwrap(session) {
                let _ = s.disconnect().await;
            }
        });
        Ok(task.to_string())
    }

    pub fn agent_cancel(&self, task_id: &str) -> Result<bool, CommandError> {
        let task: TaskId = task_id
            .parse()
            .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid task id"))?;
        Ok(self.agent.cancel_task(task))
    }

    /// Records the operator's decision. For a root command the operator may give
    /// a sudo password; it is checked and held in memory before the decision is
    /// recorded, and the task takes it only after the database confirmed the
    /// approval. It never reaches the model, the database or a log.
    pub fn approval_decide(
        &self,
        approval_id: &str,
        approve: bool,
        sudo: Option<SudoAnswerInput>,
    ) -> Result<(), CommandError> {
        self.approval_decide_with_root_scope(approval_id, approve, sudo, false)
    }

    pub fn approval_decide_with_root_scope(
        &self,
        approval_id: &str,
        approve: bool,
        sudo: Option<SudoAnswerInput>,
        approve_root_for_task: bool,
    ) -> Result<(), CommandError> {
        let id: ApprovalId = approval_id
            .parse()
            .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid approval id"))?;
        let decision = if approve {
            ApprovalDecision::Approve
        } else {
            ApprovalDecision::Deny
        };
        let task_grant = if approve_root_for_task {
            if !approve {
                return Err(CommandError::new(
                    ErrorCode::InvalidInput,
                    "root commands can only be enabled when approving a command",
                ));
            }
            let db = self.lock_db_pub()?;
            let record = db
                .get_approval(id)
                .map_err(|e| storage(&e))?
                .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "approval not found"))?;
            let execution = db
                .get_execution(record.execution)
                .map_err(|e| storage(&e))?
                .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "command not found"))?;
            if !execution.command_redacted.starts_with("sudo ") {
                return Err(CommandError::new(
                    ErrorCode::InvalidInput,
                    "task root approval can only be enabled from a sudo command",
                ));
            }
            Some((record.task, record.execution))
        } else {
            None
        };
        let held = match (approve, sudo) {
            (true, Some(answer)) => {
                let password = self.resolve_sudo_password(id, &answer)?;
                self.agent.hold_sudo_password(id, password);
                true
            }
            _ => false,
        };
        let recorded = self.lock_db_pub().and_then(|mut db| {
            let result = if approve_root_for_task {
                db.decide_approval_with_root_scope(id, decision, now_ms())
            } else {
                db.decide_approval(id, decision, now_ms())
            };
            result.map_err(|e| storage(&e))
        });
        if let Err(e) = recorded {
            if held {
                drop(self.agent.take_sudo_password(id));
            }
            return Err(e);
        }
        if let Some((task, execution)) = task_grant {
            let task_password = self.agent.copy_sudo_password(id);
            self.agent
                .approve_root_for_task(task, execution, task_password);
        }
        self.agent.deliver(id, decision);
        Ok(())
    }

    /// Which password sources the approval card may offer.
    pub fn sudo_options(&self, approval_id: &str) -> Result<SudoOptionsView, CommandError> {
        let id: ApprovalId = approval_id
            .parse()
            .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid approval id"))?;
        let host = self.approval_host(id)?;
        let record = self
            .lock_db_pub()?
            .get_host(host)
            .map_err(|e| storage(&e))?
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "host not found"))?;
        let saved = self
            .secrets
            .get(&SecretKey::ssh(host, SshSecretKind::SudoPassword))
            .map_err(|e| CommandError::new(e.code(), e.to_string()))?
            .is_some();
        Ok(SudoOptionsView {
            host_label: record.label.as_str().to_owned(),
            saved_password: saved,
            login_password: record.auth == AuthMethod::Password && record.credential_configured,
        })
    }

    /// Deletes the sudo password saved for a host, if any.
    pub fn host_forget_sudo_password(&self, host_id: &str) -> Result<(), CommandError> {
        let host: HostId = host_id
            .parse()
            .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid host id"))?;
        self.secrets
            .delete(&SecretKey::ssh(host, SshSecretKind::SudoPassword))
            .map_err(|e| CommandError::new(e.code(), e.to_string()))
    }

    fn approval_host(&self, id: ApprovalId) -> Result<HostId, CommandError> {
        Ok(self
            .lock_db_pub()?
            .get_approval(id)
            .map_err(|e| storage(&e))?
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "approval not found"))?
            .host)
    }

    fn resolve_sudo_password(
        &self,
        approval: ApprovalId,
        answer: &SudoAnswerInput,
    ) -> Result<SecretValue, CommandError> {
        let host = self.approval_host(approval)?;
        let secret_error =
            |e: selai_secrets::SecretError| CommandError::new(e.code(), e.to_string());
        let missing = |what: &str| {
            CommandError::new(
                ErrorCode::SudoPasswordRequired,
                format!("no {what} is saved for this host"),
            )
        };
        match answer.source.as_str() {
            "typed" => {
                let text = answer.password.as_deref().unwrap_or("");
                if text.is_empty()
                    || text.len() > SUDO_PASSWORD_MAX_BYTES
                    || text.contains(['\n', '\r', '\0'])
                {
                    return Err(CommandError::new(
                        ErrorCode::InvalidInput,
                        "the sudo password must be 1 to 1024 bytes on one line",
                    ));
                }
                let secret = SecretValue::new(text);
                if answer.remember == Some(true) {
                    self.secrets
                        .put(&SecretKey::ssh(host, SshSecretKind::SudoPassword), &secret)
                        .map_err(secret_error)?;
                }
                Ok(secret)
            }
            "saved" => self
                .secrets
                .get(&SecretKey::ssh(host, SshSecretKind::SudoPassword))
                .map_err(secret_error)?
                .ok_or_else(|| missing("sudo password")),
            "login" => {
                let record = self
                    .lock_db_pub()?
                    .get_host(host)
                    .map_err(|e| storage(&e))?
                    .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "host not found"))?;
                if record.auth != AuthMethod::Password {
                    return Err(CommandError::new(
                        ErrorCode::InvalidInput,
                        "this host signs in with a key, so there is no login password to reuse",
                    ));
                }
                self.secrets
                    .get(&SecretKey::ssh(host, SshSecretKind::Password))
                    .map_err(secret_error)?
                    .ok_or_else(|| missing("SSH password"))
            }
            _ => Err(CommandError::new(
                ErrorCode::InvalidInput,
                "the sudo answer must be typed, saved or login",
            )),
        }
    }

    pub fn history_list(
        &self,
        host_id: &str,
        cursor: Option<String>,
    ) -> Result<Page<ExecutionView>, CommandError> {
        let host: HostId = host_id
            .parse()
            .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid host id"))?;
        let db = self.lock_db_pub()?;
        let page = db
            .host_command_history(host, &PageRequest { cursor, limit: 50 })
            .map_err(|e| storage(&e))?;
        Ok(Page {
            items: page.items.into_iter().map(execution_view).collect(),
            next_cursor: page.next_cursor,
        })
    }

    pub fn audit_verify(&self) -> Result<AuditVerifyView, CommandError> {
        let db: std::sync::MutexGuard<'_, Database> = self.lock_db_pub()?;
        match db.verify_audit_chain().map_err(|e| storage(&e))? {
            Ok(count) => Ok(AuditVerifyView {
                ok: true,
                events_checked: count,
                broken_at_seq: None,
            }),
            Err(err) => {
                let seq = match err {
                    selai_audit::ChainError::Broken { seq }
                    | selai_audit::ChainError::Sequence { seq } => seq,
                };
                Ok(AuditVerifyView {
                    ok: false,
                    events_checked: 0,
                    broken_at_seq: Some(seq),
                })
            }
        }
    }
}

/// Converts a task event into the wire shape, tagged with its task.
fn wire_event(task: TaskId, event: TaskEvent) -> AgentEventView {
    AgentEventView {
        task_id: task.to_string(),
        event: event_view(event),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Arc<AgentRegistry> {
        Arc::new(AgentRegistry::default())
    }

    #[tokio::test]
    async fn a_decision_delivered_before_the_wait_is_not_lost() {
        let registry = registry();
        let approval = ApprovalId::new();
        registry.deliver(approval, ApprovalDecision::Approve);

        let waiter = RegistryWaiter {
            registry: Arc::clone(&registry),
        };
        let outcome = waiter
            .wait(approval, Duration::from_secs(5), &CancellationToken::new())
            .await;
        assert_eq!(outcome, WaitOutcome::Decided(ApprovalDecision::Approve));
    }

    #[tokio::test]
    async fn a_decision_delivered_during_the_wait_wakes_it() {
        let registry = registry();
        let approval = ApprovalId::new();
        let waiter = RegistryWaiter {
            registry: Arc::clone(&registry),
        };
        let handle = tokio::spawn({
            let registry = Arc::clone(&registry);
            async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                registry.deliver(approval, ApprovalDecision::Deny);
            }
        });
        let outcome = waiter
            .wait(approval, Duration::from_secs(5), &CancellationToken::new())
            .await;
        handle.await.expect("deliver task");
        assert_eq!(outcome, WaitOutcome::Decided(ApprovalDecision::Deny));
    }

    #[tokio::test]
    async fn no_decision_times_out_and_leaves_no_waiter_behind() {
        let registry = registry();
        let approval = ApprovalId::new();
        let waiter = RegistryWaiter {
            registry: Arc::clone(&registry),
        };
        let outcome = waiter
            .wait(
                approval,
                Duration::from_millis(10),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(outcome, WaitOutcome::TimedOut);
        assert!(registry.approvals.lock().expect("lock").waiters.is_empty());
    }
}
