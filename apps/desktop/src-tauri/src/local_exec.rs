//! Agent commands on this computer ("This computer" target). Same contract as
//! the SSH executor: no stdin, both streams capped, a timeout or cancel kills
//! the process, and a root command goes through the same sudo wrapper, whose
//! password is written only on this run's unique prompt.

use std::{future::Future, process::Stdio, time::Duration};

use selai_agent::RemoteExecutor;
use selai_secrets::SecretValue;
use selai_ssh::{
    sudo::{strip_prompt, sudo_plan, PromptWatch, StdinAction, SUDO_PROBE},
    ExecEnd, ExecOutput, SshError, SudoStatus,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct LocalExecutor {
    distribution: Option<String>,
}

impl LocalExecutor {
    pub fn new(distribution: Option<String>) -> Result<Self, SshError> {
        if let Some(name) = &distribution {
            crate::wsl::validate(name).map_err(|error| SshError::Protocol(error.message))?;
        }
        Ok(Self { distribution })
    }
}

/// The shell that runs one command string.
fn shell(command: &str, distribution: Option<&str>) -> Command {
    if let Some(distribution) = distribution {
        let mut command_builder = Command::new(crate::wsl::executable());
        command_builder.args([
            "--distribution",
            distribution,
            "--cd",
            "~",
            "--exec",
            "sh",
            "-c",
            command,
        ]);
        return command_builder;
    }
    #[cfg(windows)]
    {
        let mut c = Command::new("powershell.exe");
        c.args(["-NoProfile", "-NonInteractive", "-Command", command]);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    }
}

fn spawn_error(e: &std::io::Error) -> SshError {
    SshError::Protocol(format!("the local command could not start: {e}"))
}

impl RemoteExecutor for LocalExecutor {
    fn run(
        &self,
        command: &str,
        timeout: Duration,
        max_bytes_per_stream: usize,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<ExecOutput, SshError>> + Send {
        let command = command.to_owned();
        let cancel = cancel.clone();
        async move {
            let child = spawn(&command, false, self.distribution.as_deref())?;
            Ok(collect(child, None, timeout, max_bytes_per_stream, &cancel).await)
        }
    }

    async fn sudo_status(&self, timeout: Duration) -> Result<SudoStatus, SshError> {
        if cfg!(windows) && self.distribution.is_none() {
            return Ok(SudoStatus::Unavailable);
        }
        let child = spawn(SUDO_PROBE, false, self.distribution.as_deref())?;
        let out = collect(child, None, timeout, 4096, &CancellationToken::new()).await;
        Ok(SudoStatus::from_probe(
            out.exit_status,
            &String::from_utf8_lossy(&out.stderr),
        ))
    }

    fn run_as_root(
        &self,
        command: &str,
        password: Option<&SecretValue>,
        timeout: Duration,
        max_bytes_per_stream: usize,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<ExecOutput, SshError>> + Send {
        let plan = sudo_plan(command, password.is_some());
        let password = password.map(|p| SecretValue::new(p.expose_for_transport().to_vec()));
        let cancel = cancel.clone();
        async move {
            if cfg!(windows) && self.distribution.is_none() {
                return Err(SshError::Protocol(
                    "running as root is not supported on Windows".to_owned(),
                ));
            }
            let feed = match (&plan.prompt, password) {
                (Some(prompt), Some(secret)) => Some((PromptWatch::new(prompt), secret)),
                _ => None,
            };
            let child = spawn(&plan.command, feed.is_some(), self.distribution.as_deref())?;
            let mut out = collect(child, feed, timeout, max_bytes_per_stream, &cancel).await;
            if let Some(prompt) = &plan.prompt {
                out.stderr = strip_prompt(&out.stderr, prompt);
            }
            Ok(out)
        }
    }
}

fn spawn(command: &str, with_stdin: bool, distribution: Option<&str>) -> Result<Child, SshError> {
    let mut cmd = shell(command, distribution);
    cmd.stdin(if with_stdin {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    #[cfg(windows)]
    {
        // Selai is a GUI application without a console. Without this flag,
        // PowerShell allocates a new console (often in Windows Terminal) for
        // each AI command instead of keeping its output on the IPC pipes.
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    if let Some(home) = home_dir() {
        cmd.current_dir(home);
    }
    cmd.spawn().map_err(|e| spawn_error(&e))
}

pub(crate) fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(Into::into)
}

/// Reads both streams until the process exits, the timeout passes, or the
/// caller cancels. With `feed`, answers the sudo prompt once and closes stdin.
async fn collect(
    mut child: Child,
    mut feed: Option<(PromptWatch, SecretValue)>,
    timeout: Duration,
    cap: usize,
    cancel: &CancellationToken,
) -> ExecOutput {
    let mut out = ExecOutput {
        stdout: Vec::new(),
        stdout_truncated: false,
        stderr: Vec::new(),
        stderr_truncated: false,
        exit_status: None,
        exit_signal: None,
        end: ExecEnd::Completed,
    };
    let mut stdin: Option<ChildStdin> = child.stdin.take();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut out_buf = [0u8; 8192];
    let mut err_buf = [0u8; 8192];
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    while stdout.is_some() || stderr.is_some() {
        tokio::select! {
            () = cancel.cancelled() => {
                out.end = ExecEnd::Cancelled;
                break;
            }
            () = &mut deadline => {
                out.end = ExecEnd::TimedOut;
                break;
            }
            n = read_some(&mut stdout, &mut out_buf) => match n {
                Some(n) => {
                    let data = &out_buf[..n];
                    if let Some((watch, _)) = feed.as_mut() {
                        if watch.on_stdout(data) == StdinAction::Close {
                            stdin = None;
                        }
                    }
                    append_capped(&mut out.stdout, &mut out.stdout_truncated, data, cap);
                }
                None => stdout = None,
            },
            n = read_some(&mut stderr, &mut err_buf) => match n {
                Some(n) => {
                    let data = &err_buf[..n];
                    if let Some((watch, secret)) = feed.as_mut() {
                        if watch.on_stderr(data) == StdinAction::SendPasswordAndClose {
                            if let Some(mut pipe) = stdin.take() {
                                let mut line = secret.expose_for_transport().to_vec();
                                line.push(b'\n');
                                let _ = pipe.write_all(&line).await;
                                line.fill(0);
                                drop(pipe);
                            }
                        }
                    }
                    append_capped(&mut out.stderr, &mut out.stderr_truncated, data, cap);
                }
                None => stderr = None,
            },
        }
    }
    drop(stdin);
    if out.end == ExecEnd::Completed {
        match child.wait().await {
            Ok(status) => {
                out.exit_status = status.code().and_then(|c| u32::try_from(c).ok());
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    out.exit_signal = status.signal().map(|s| format!("signal {s}"));
                }
                if out.exit_status.is_none() && out.exit_signal.is_none() {
                    out.end = ExecEnd::ChannelClosed;
                }
            }
            Err(_) => out.end = ExecEnd::ChannelClosed,
        }
    } else {
        let _ = child.kill().await;
    }
    out
}

/// One read from an optional pipe. A closed pipe never resolves again, so the
/// select loop waits on the other one.
async fn read_some<R: tokio::io::AsyncRead + Unpin>(
    pipe: &mut Option<R>,
    buf: &mut [u8],
) -> Option<usize> {
    match pipe {
        Some(p) => match p.read(buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(n),
        },
        None => std::future::pending().await,
    }
}

fn append_capped(buf: &mut Vec<u8>, truncated: &mut bool, data: &[u8], cap: usize) {
    let room = cap.saturating_sub(buf.len());
    if data.len() > room {
        buf.extend_from_slice(&data[..room]);
        *truncated = true;
    } else {
        buf.extend_from_slice(data);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn runs_with_separate_streams_and_the_exit_status() {
        let out = LocalExecutor::default()
            .run(
                "printf out; printf err >&2; exit 3",
                LIMIT,
                1024,
                &CancellationToken::new(),
            )
            .await
            .expect("run");
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, b"err");
        assert_eq!(out.exit_status, Some(3));
        assert_eq!(out.end, ExecEnd::Completed);
    }

    #[tokio::test]
    async fn stdin_is_closed_so_a_reader_does_not_hang() {
        let out = LocalExecutor::default()
            .run("cat; echo done", LIMIT, 1024, &CancellationToken::new())
            .await
            .expect("run");
        assert_eq!(out.stdout, b"done\n");
    }

    #[tokio::test]
    async fn output_is_capped_and_a_timeout_kills_the_process() {
        let out = LocalExecutor::default()
            .run(
                "yes | head -c 100000",
                LIMIT,
                1000,
                &CancellationToken::new(),
            )
            .await
            .expect("run");
        assert_eq!(out.stdout.len(), 1000);
        assert!(out.stdout_truncated);

        let slow = LocalExecutor::default()
            .run(
                "sleep 30",
                Duration::from_millis(200),
                1024,
                &CancellationToken::new(),
            )
            .await
            .expect("run");
        assert_eq!(slow.end, ExecEnd::TimedOut);
    }
}
