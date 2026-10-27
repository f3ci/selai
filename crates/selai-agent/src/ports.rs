//! Ports the runtime depends on: remote execution, approval waiting, and the
//! event stream shown to the operator. Implementations live in the app crate
//! (and in tests).

use std::{future::Future, sync::Arc, time::Duration};

use selai_domain::{ApprovalId, ExecutionId, TaskId};
use selai_secrets::SecretValue;
use selai_ssh::{ExecOutput, SshError, SudoStatus};
use selai_storage::ApprovalDecision;
use tokio_util::sync::CancellationToken;

/// Runs one non-interactive command on the task's host.
pub trait RemoteExecutor: Send + Sync {
    fn run(
        &self,
        command: &str,
        timeout: Duration,
        max_bytes_per_stream: usize,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<ExecOutput, SshError>> + Send;

    /// Whether `sudo` on the task's host needs a password. Runs nothing as root.
    fn sudo_status(
        &self,
        timeout: Duration,
    ) -> impl Future<Output = Result<SudoStatus, SshError>> + Send;

    /// Runs `command` as root through `sudo`. The password, when given, goes to
    /// sudo's prompt only and never into the command text or the output.
    fn run_as_root(
        &self,
        command: &str,
        password: Option<&SecretValue>,
        timeout: Duration,
        max_bytes_per_stream: usize,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<ExecOutput, SshError>> + Send;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The operator decided. The decision is already in the database; the runtime
    /// still re-reads it before acting.
    Decided(ApprovalDecision),
    TimedOut,
    Cancelled,
}

/// Waits for the operator to decide an approval ticket.
pub trait ApprovalWaiter: Send + Sync {
    fn wait(
        &self,
        approval: ApprovalId,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> impl Future<Output = WaitOutcome> + Send;

    /// The sudo password the operator gave with this approval, if any. Taking
    /// it removes it, so it is used for one command only.
    fn take_sudo_password(&self, _approval: ApprovalId) -> Option<SecretValue> {
        None
    }

    /// Whether the operator enabled command approval for this live task.
    fn task_command_approved(&self, _task: TaskId, _execution: ExecutionId) -> bool {
        false
    }

    /// Returns the task-scoped sudo password, if one was provided with the
    /// approval that enabled root commands. Implementations must keep it only
    /// in memory and clear it when the task ends or the vault locks.
    fn task_sudo_password(&self, _task: TaskId) -> Option<SecretValue> {
        None
    }
}

/// What the operator sees while a task runs.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskEvent {
    State {
        state: String,
        error_code: Option<String>,
    },
    TextDelta {
        text: String,
    },
    ToolProposed {
        execution: ExecutionId,
        tool: String,
        command: String,
        host_label: String,
    },
    ApprovalRequired {
        approval: ApprovalId,
        execution: ExecutionId,
        command: String,
        expires_at_ms: i64,
        /// Set for a root command: what the host's sudo needs.
        sudo: Option<SudoStatus>,
    },
    ToolFinished {
        execution: ExecutionId,
        state: String,
        exit_code: Option<i64>,
        error_code: Option<String>,
    },
    /// Running token totals for the task after a model turn. `None` when the
    /// provider has not reported that count.
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

pub type EventSink = Arc<dyn Fn(TaskEvent) + Send + Sync>;
