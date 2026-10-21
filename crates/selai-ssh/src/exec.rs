//! Non-interactive command execution on a separate channel (ADR-0002, ADR-0005).
//!
//! stdout and stderr are kept apart, the exit status is reported when the server
//! sends one, and each stream is capped. A timeout or cancellation closes the
//! channel. The remote process may keep running; the caller must report that.

use std::time::Duration;

use russh::{ChannelMsg, Sig};
use tokio_util::sync::CancellationToken;

use selai_secrets::SecretValue;

use crate::{
    sudo::{strip_prompt, sudo_plan, PromptWatch, StdinAction, SudoStatus, SUDO_PROBE},
    Session, SshError,
};

/// Extended-data type used by OpenSSH for stderr.
const STDERR_EXT: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecEnd {
    /// The channel closed on its own, with an exit status.
    Completed,
    /// The timeout elapsed. The channel was closed.
    TimedOut,
    /// The caller cancelled. The channel was closed.
    Cancelled,
    /// The channel closed without an exit status.
    ChannelClosed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr: Vec<u8>,
    pub stderr_truncated: bool,
    pub exit_status: Option<u32>,
    /// Set when the server reported a signal rather than an exit status.
    pub exit_signal: Option<String>,
    pub end: ExecEnd,
}

impl Session {
    /// Runs one command on a new exec channel (no PTY).
    ///
    /// `command` is passed to the remote shell as a string. Callers must have
    /// built it from validated parts (ADR-0005). It is not argv-safe.
    pub async fn exec(
        &self,
        command: &str,
        timeout: Duration,
        max_bytes_per_stream: usize,
        cancel: &CancellationToken,
    ) -> Result<ExecOutput, SshError> {
        self.exec_inner(command, None, timeout, max_bytes_per_stream, cancel)
            .await
    }

    /// Asks the host whether `sudo` needs a password. Runs nothing as root.
    pub async fn sudo_status(&self, timeout: Duration) -> Result<SudoStatus, SshError> {
        let out = self
            .exec_inner(SUDO_PROBE, None, timeout, 4096, &CancellationToken::new())
            .await?;
        Ok(SudoStatus::from_probe(
            out.exit_status,
            &String::from_utf8_lossy(&out.stderr),
        ))
    }

    /// Runs `inner` as root through `sudo` (see [`crate::sudo`]). With a password,
    /// it is written to stdin only after sudo prints this run's unique prompt.
    /// Without one, sudo must not prompt and fails instead.
    pub async fn exec_as_root(
        &self,
        inner: &str,
        password: Option<&SecretValue>,
        timeout: Duration,
        max_bytes_per_stream: usize,
        cancel: &CancellationToken,
    ) -> Result<ExecOutput, SshError> {
        let plan = sudo_plan(inner, password.is_some());
        let feed = match (&plan.prompt, password) {
            (Some(prompt), Some(secret)) => Some((PromptWatch::new(prompt), secret)),
            _ => None,
        };
        let mut out = self
            .exec_inner(&plan.command, feed, timeout, max_bytes_per_stream, cancel)
            .await?;
        if let Some(prompt) = &plan.prompt {
            out.stderr = strip_prompt(&out.stderr, prompt);
        }
        Ok(out)
    }

    async fn exec_inner(
        &self,
        command: &str,
        mut feed: Option<(PromptWatch, &SecretValue)>,
        timeout: Duration,
        max_bytes_per_stream: usize,
        cancel: &CancellationToken,
    ) -> Result<ExecOutput, SshError> {
        if let Some(native) = &self.native {
            let process = tokio::select! {
                biased;
                _=cancel.cancelled()=>return Ok(ExecOutput{stdout:Vec::new(),stderr:Vec::new(),stdout_truncated:false,stderr_truncated:false,exit_status:None,exit_signal:None,end:ExecEnd::Cancelled}),
                process=native.process(&[],Some(command))=>process?,
            };
            return collect_native(process, feed, timeout, max_bytes_per_stream, cancel).await;
        }
        let mut channel = self.handle()?.channel_open_session().await?;
        channel.exec(true, command.as_bytes().to_vec()).await?;

        let mut out = ExecOutput {
            stdout: Vec::new(),
            stdout_truncated: false,
            stderr: Vec::new(),
            stderr_truncated: false,
            exit_status: None,
            exit_signal: None,
            end: ExecEnd::Completed,
        };
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    out.end = ExecEnd::Cancelled;
                    break;
                }
                () = &mut deadline => {
                    out.end = ExecEnd::TimedOut;
                    break;
                }
                msg = channel.wait() => match msg {
                    Some(ChannelMsg::Data { data }) => {
                        if let Some((watch, _)) = feed.as_mut() {
                            if watch.on_stdout(&data) == StdinAction::Close {
                                let _ = channel.eof().await;
                            }
                        }
                        append_capped(&mut out.stdout, &mut out.stdout_truncated, &data, max_bytes_per_stream);
                    }
                    Some(ChannelMsg::ExtendedData { data, ext }) if ext == STDERR_EXT => {
                        if let Some((watch, secret)) = feed.as_mut() {
                            if watch.on_stderr(&data) == StdinAction::SendPasswordAndClose {
                                let mut line = secret.expose_for_transport().to_vec();
                                line.push(b'\n');
                                let sent = channel.data(&line[..]).await;
                                line.fill(0);
                                let _ = channel.eof().await;
                                if sent.is_err() {
                                    return Err(SshError::ChannelClosed);
                                }
                            }
                        }
                        append_capped(&mut out.stderr, &mut out.stderr_truncated, &data, max_bytes_per_stream);
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        out.exit_status = Some(exit_status);
                    }
                    Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                        out.exit_signal = Some(signal_name_text(&signal_name));
                    }
                    Some(ChannelMsg::Failure) => {
                        return Err(SshError::Protocol("server refused to run the command".to_owned()));
                    }
                    Some(ChannelMsg::Close) | None => {
                        out.end = if out.exit_status.is_some() || out.exit_signal.is_some() {
                            ExecEnd::Completed
                        } else {
                            ExecEnd::ChannelClosed
                        };
                        break;
                    }
                    Some(_) => {}
                }
            }
        }
        if out.end != ExecEnd::Completed && out.end != ExecEnd::ChannelClosed {
            // Best effort. The remote process may still run; the caller reports that.
            let _ = channel.close().await;
        }
        Ok(out)
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

fn signal_name_text(signal: &Sig) -> String {
    format!("{signal:?}")
}

async fn collect_native(
    mut process: crate::native::NativeProcess,
    mut feed: Option<(PromptWatch, &SecretValue)>,
    timeout: Duration,
    cap: usize,
    cancel: &CancellationToken,
) -> Result<ExecOutput, SshError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut out = ExecOutput {
        stdout: Vec::new(),
        stderr: Vec::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        exit_status: None,
        exit_signal: None,
        end: ExecEnd::Completed,
    };
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut stdout = [0; 8192];
    let mut stderr = [0; 8192];
    if feed.is_none() {
        process.stdin.take();
    }
    loop {
        tokio::select! {
            biased;
            _=cancel.cancelled()=>{out.end=ExecEnd::Cancelled;break},
            _=&mut deadline=>{out.end=ExecEnd::TimedOut;break},
            result=process.stdout.read(&mut stdout), if stdout_open=>match result {
                Ok(0)|Err(_)=>stdout_open=false,
                Ok(n)=>{if let Some((watch,_))=feed.as_mut(){if watch.on_stdout(&stdout[..n])==StdinAction::Close{process.stdin.take();}} append_capped(&mut out.stdout,&mut out.stdout_truncated,&stdout[..n],cap);}
            },
            result=async {match process.stderr.as_mut(){Some(pipe)=>pipe.read(&mut stderr).await,None=>Ok(0)}}, if stderr_open=>match result {
                Ok(0)|Err(_)=>stderr_open=false,
                Ok(n)=>{
                    if let Some((watch,secret))=feed.as_mut(){if watch.on_stderr(&stderr[..n])==StdinAction::SendPasswordAndClose{
                        if let Some(mut stdin)=process.stdin.take(){let mut line=secret.expose_for_transport().to_vec();line.push(b'\n');let sent=stdin.write_all(&line).await;line.fill(0);sent?;}
                    }}
                    append_capped(&mut out.stderr,&mut out.stderr_truncated,&stderr[..n],cap);
                }
            },
            result=process.exit.changed(), if !stdout_open && !stderr_open && process.exit.borrow().is_none()=>{if result.is_err(){out.end=ExecEnd::ChannelClosed;break}},
        }
        if !stdout_open && !stderr_open {
            if let Some(status) = *process.exit.borrow() {
                out.exit_status = Some(status);
                break;
            }
        }
    }
    Ok(out)
}
