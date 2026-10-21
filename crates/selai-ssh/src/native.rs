//! System OpenSSH backend for OS agents, certificates, hardware keys and SSO.
//! Every process receives an isolated known_hosts file containing Selai's pins.
use crate::{ConnectOptions, ShellEvent, SshError};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::{
    io::{Read, Write},
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, BufReader, ReadBuf},
    process::{ChildStderr, ChildStdin, ChildStdout, Command},
    sync::{mpsc, watch},
};
use tokio_util::sync::CancellationToken;

/// Transient authentication instructions. An empty message clears the previous
/// connection's context. Implementations must not persist or log this text.
pub trait NativeInteraction: Send + Sync {
    fn message(&self, _text: &str) {}
}
impl NativeInteraction for () {}

/// Non-secret native authentication settings. The lease revokes askpass access
/// when the last session/process drops it. Responses never appear in argv/env.
#[derive(Clone)]
pub struct NativeAuth {
    pub method: String,
    pub identity: Option<PathBuf>,
    pub helper: PathBuf,
    pub environment: Vec<(String, String)>,
    pub lease: Arc<dyn NativeInteraction>,
    pub cancel: CancellationToken,
}

pub trait TransportStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> TransportStream for T {}
pub type SshStream = Box<dyn TransportStream>;

pub(crate) struct NativeSession {
    auth: NativeAuth,
    args: Vec<String>,
    _pins: tempfile::TempDir,
    pub stop: CancellationToken,
    closed: watch::Receiver<Option<u32>>,
    forwards: Mutex<std::collections::HashMap<(String, u16), CancellationToken>>,
}
impl Drop for NativeSession {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn option(args: &mut Vec<String>, key: &str, value: impl std::fmt::Display) {
    args.extend(["-o".into(), format!("{key}={value}")]);
}

fn arguments(opts: &ConnectOptions, pin_file: &std::path::Path) -> Result<Vec<String>, SshError> {
    let crate::Auth::Native(auth) = &opts.auth else {
        return Err(SshError::AuthFailed);
    };
    let mut args = vec![
        "-p".into(),
        opts.endpoint.port.to_string(),
        "-l".into(),
        opts.username.clone(),
    ];
    for (key, value) in [
        ("StrictHostKeyChecking", "yes"),
        ("BatchMode", "no"),
        (
            "GlobalKnownHostsFile",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        ),
        ("HostKeyAlias", "selai-pinned"),
        ("CheckHostIP", "no"),
        ("VerifyHostKeyDNS", "no"),
        ("UpdateHostKeys", "no"),
        ("ControlMaster", "no"),
        ("ControlPath", "none"),
        ("ForwardAgent", "no"),
        ("ForwardX11", "no"),
        ("ClearAllForwardings", "yes"),
        ("PermitLocalCommand", "no"),
        ("NumberOfPasswordPrompts", "3"),
    ] {
        option(&mut args, key, value);
    }
    // OpenSSH supports quotes around whitespace-containing option values.
    let path = pin_file.to_string_lossy();
    if path.contains(['"', '\n', '\r']) {
        return Err(SshError::Protocol("Invalid known-hosts path".into()));
    }
    option(&mut args, "UserKnownHostsFile", format!("\"{path}\""));
    option(
        &mut args,
        "ConnectTimeout",
        opts.connect_timeout.as_secs().max(1),
    );
    option(
        &mut args,
        "ServerAliveInterval",
        opts.keepalive.unwrap_or_default().as_secs(),
    );
    let mut algorithms = Vec::new();
    for pin in &opts.pinned {
        if pin.algorithm == "ssh-rsa" {
            algorithms.extend(["rsa-sha2-512".to_owned(), "rsa-sha2-256".to_owned()]);
        } else {
            algorithms.push(pin.algorithm.clone());
        }
    }
    algorithms.sort();
    algorithms.dedup();
    option(&mut args, "HostKeyAlgorithms", algorithms.join(","));
    let preferred = match auth.method.as_str() {
        "auto" => None,
        "keyboard_interactive" => Some("keyboard-interactive,password"),
        "agent" | "certificate" | "security_key" | "smart_card" => {
            Some("publickey,keyboard-interactive,password")
        }
        "gssapi" => {
            option(&mut args, "GSSAPIAuthentication", "yes");
            Some("gssapi-with-mic,keyboard-interactive,password")
        }
        "host_based" => {
            option(&mut args, "HostbasedAuthentication", "yes");
            option(&mut args, "EnableSSHKeysign", "yes");
            Some("hostbased,keyboard-interactive,password")
        }
        _ => {
            return Err(SshError::Protocol(
                "Unknown native authentication method".into(),
            ))
        }
    };
    if let Some(preferred) = preferred {
        option(&mut args, "PreferredAuthentications", preferred);
    }
    if auth.method == "agent" {
        option(&mut args, "IdentityFile", "none");
    }
    if auth.method == "certificate" {
        option(&mut args, "IdentitiesOnly", "yes");
        option(&mut args,"PubkeyAcceptedAlgorithms","ssh-ed25519-cert-v01@openssh.com,ecdsa-sha2-nistp256-cert-v01@openssh.com,ecdsa-sha2-nistp384-cert-v01@openssh.com,ecdsa-sha2-nistp521-cert-v01@openssh.com,rsa-sha2-512-cert-v01@openssh.com,rsa-sha2-256-cert-v01@openssh.com,sk-ssh-ed25519-cert-v01@openssh.com,sk-ecdsa-sha2-nistp256-cert-v01@openssh.com");
    }
    if auth.method == "security_key" {
        if auth.identity.is_some() {
            option(&mut args, "IdentitiesOnly", "yes");
        }
        option(&mut args,"PubkeyAcceptedAlgorithms","sk-ssh-ed25519@openssh.com,sk-ecdsa-sha2-nistp256@openssh.com,sk-ssh-ed25519-cert-v01@openssh.com,sk-ecdsa-sha2-nistp256-cert-v01@openssh.com,webauthn-sk-ecdsa-sha2-nistp256@openssh.com");
    }
    if auth.method == "smart_card" {
        option(&mut args, "IdentityFile", "none");
        option(&mut args, "IdentityAgent", "none");
    }
    if let Some(path) = &auth.identity {
        if path.to_string_lossy().starts_with("vault://") {
            return Err(SshError::KeyLoad("Native authentication needs a local identity/provider file or a key loaded in your SSH agent.".into()));
        }
        args.extend([
            if auth.method == "smart_card" {
                "-I"
            } else {
                "-i"
            }
            .into(),
            path.to_string_lossy().into_owned(),
        ]);
    }
    Ok(args)
}

fn ssh_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("SELAI_SSH_EXECUTABLE")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return path;
    }
    #[cfg(windows)]
    {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
            .join("System32/OpenSSH/ssh.exe")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/usr/bin/ssh")
    }
}

impl NativeSession {
    pub async fn connect(
        opts: &ConnectOptions,
        proxy: Option<crate::ProxyOptions>,
    ) -> Result<Arc<Self>, SshError> {
        let observed = crate::host_key::probe_host_key_via_ref(
            &opts.endpoint,
            opts.connect_timeout,
            proxy.as_ref(),
        )
        .await?;
        if opts.pinned.is_empty() {
            return Err(SshError::HostKeyUnknown(observed));
        }
        if !opts.pinned.iter().any(|pin| {
            pin.algorithm == observed.algorithm
                && pin.public_key_base64 == observed.public_key_base64
        }) {
            return Err(SshError::HostKeyChanged {
                observed,
                pinned_fingerprint: opts.pinned[0].fingerprint_sha256.clone(),
            });
        }
        let crate::Auth::Native(original_auth) = &opts.auth else {
            return Err(SshError::AuthFailed);
        };
        let mut auth = original_auth.clone();
        let pins = tempfile::Builder::new().prefix("selai-ssh-").tempdir()?;
        protect_pin_directory(pins.path()).await?;
        let pin_file = pins.path().join("known_hosts");
        let contents = opts
            .pinned
            .iter()
            .map(|p| format!("selai-pinned {} {}\n", p.algorithm, p.public_key_base64))
            .collect::<String>();
        std::fs::write(&pin_file, contents)?;
        let mut args = arguments(opts, &pin_file)?;
        auth.lease.message("");
        let stop = auth.cancel.clone();
        if let Some(proxy) = proxy {
            let proxy = Arc::new(proxy);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?.to_string();
            let token = auth
                .environment
                .iter()
                .find(|(k, _)| k == "SELAI_SSH_AUTH_TOKEN")
                .map(|(_, v)| v.clone())
                .ok_or(SshError::AuthFailed)?;
            auth.environment
                .push(("SELAI_SSH_PROXY_ADDR".into(), address));
            let executable = auth.helper.to_string_lossy();
            #[cfg(unix)]
            let quoted = format!("'{}'", executable.replace('\'', "'\\''"));
            #[cfg(windows)]
            let quoted = format!("\"{executable}\"");
            option(
                &mut args,
                "ProxyCommand",
                format!("{quoted} --selai-ssh-proxy"),
            );
            let endpoint = opts.endpoint.clone();
            let cancelled = stop.clone();
            tokio::spawn(async move {
                let budget = Arc::new(tokio::sync::Semaphore::new(64));
                loop {
                    let accepted = tokio::select! { biased; _=cancelled.cancelled()=>break, r=listener.accept()=>r };
                    let Ok((mut socket, _)) = accepted else { break };
                    let Ok(permit) = budget.clone().try_acquire_owned() else {
                        continue;
                    };
                    let token = token.clone();
                    let proxy = proxy.clone();
                    let endpoint = endpoint.clone();
                    let cancelled = cancelled.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let bridge = async {
                            let mut supplied = vec![0; token.len()];
                            tokio::time::timeout(
                                Duration::from_secs(5),
                                socket.read_exact(&mut supplied),
                            )
                            .await
                            .map_err(|_| SshError::ConnectTimeout)??;
                            if supplied != token.as_bytes() {
                                return Ok::<(), SshError>(());
                            }
                            let mut remote =
                                crate::proxy::transport(&endpoint, Some(&proxy)).await?;
                            tokio::io::copy_bidirectional(&mut socket, &mut remote).await?;
                            Ok(())
                        };
                        tokio::select! {_=cancelled.cancelled()=>{},_=tokio::time::timeout(Duration::from_secs(86400), bridge)=>{}}
                    });
                }
            });
        }
        args.push(opts.endpoint.host.clone());
        let (closed_tx, closed) = watch::channel(None);
        let session = Arc::new(Self {
            auth,
            args,
            _pins: pins,
            stop,
            closed,
            forwards: Mutex::new(Default::default()),
        });
        let mut command = session.command(&["-N"], true);
        let mut child = command.spawn().map_err(native_spawn_error)?;
        let stderr = child.stderr.take().ok_or(SshError::ChannelClosed)?;
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let cancelled = session.stop.clone();
        let interaction = session.auth.lease.clone();
        // Do not keep Session alive in its own monitor task.
        tokio::spawn(async move {
            let mut ready = Some(ready_tx);
            let mut lines = BufReader::new(stderr).lines();
            let mut last_error = String::new();
            loop {
                tokio::select! {
                    biased;
                    _=cancelled.cancelled()=>{let _=child.kill().await; break;},
                    line=lines.next_line()=>match line {
                        Ok(Some(line))=>{
                            if line.starts_with("Authenticated to ") || line.contains("Authentication succeeded (") {
                                if let Some(tx)=ready.take(){let _=tx.send(Ok(()));}
                            }
                            if !line.starts_with("debug") && !line.is_empty(){interaction.message(&format!("{line}\n"));}
                            if !line.starts_with("debug") && !line.is_empty() && last_error.len()<8192 {last_error.push_str(&line.chars().take(2048).collect::<String>());last_error.push('\n');}
                        },
                        _=>{let status=child.wait().await.ok().and_then(|s|s.code()).and_then(|s|u32::try_from(s).ok()); let _=closed_tx.send(Some(status.unwrap_or(255))); break;}
                    }
                }
            }
            if let Some(tx) = ready {
                let _ = tx.send(Err(SshError::NativeAuth(last_error)));
            }
        });
        match tokio::time::timeout(Duration::from_secs(300), ready_rx).await {
            Ok(Ok(result)) => result?,
            _ => {
                session.stop.cancel();
                return Err(SshError::ConnectTimeout);
            }
        }
        Ok(session)
    }

    fn command(&self, operation: &[&str], debug: bool) -> Command {
        let mut command = Command::new(ssh_binary());
        // Options precede the hostname, including operation flags.
        command.args([
            "-o",
            if debug {
                "LogLevel=DEBUG1"
            } else {
                "LogLevel=ERROR"
            },
        ]);
        command
            .args(operation)
            .args(&self.args[..self.args.len() - 1]);
        command.arg(&self.args[self.args.len() - 1]);
        command
            .envs(self.auth.environment.iter().cloned())
            .env("SSH_ASKPASS", &self.auth.helper)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("DISPLAY", "selai:0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        command
    }
    pub fn is_closed(&self) -> bool {
        self.stop.is_cancelled() || self.closed.borrow().is_some()
    }

    pub async fn process(
        self: &Arc<Self>,
        operation: &[&str],
        remote_command: Option<&str>,
    ) -> Result<NativeProcess, SshError> {
        if self.is_closed() {
            return Err(SshError::ChannelClosed);
        }
        let ready = if !operation.contains(&"-v")
            && self
                .auth
                .environment
                .iter()
                .any(|(key, _)| key == "SELAI_SSH_AUTH_ADDR")
        {
            Some((
                tokio::net::TcpListener::bind("127.0.0.1:0").await?,
                selai_domain::HostId::new().to_string(),
            ))
        } else {
            None
        };
        let mut flags = Vec::<String>::new();
        if ready.is_some() {
            let executable = self.auth.helper.to_string_lossy();
            #[cfg(unix)]
            let quoted = format!("'{}'", executable.replace('\'', "'\\''"));
            #[cfg(windows)]
            let quoted = format!("\"{executable}\"");
            flags.extend([
                "-o".into(),
                "PermitLocalCommand=yes".into(),
                "-o".into(),
                format!("LocalCommand={quoted} --selai-ssh-ready"),
            ]);
        }
        flags.extend(operation.iter().map(|s| s.to_string()));
        let flags = flags.iter().map(String::as_str).collect::<Vec<_>>();
        let mut command = self.command(&flags, operation.contains(&"-v"));
        if let Some((listener, token)) = &ready {
            command
                .env("SELAI_SSH_READY_ADDR", listener.local_addr()?.to_string())
                .env("SELAI_SSH_READY_TOKEN", token);
        }
        if let Some(remote_command) = remote_command {
            command.arg(remote_command);
        }
        command.stdin(Stdio::piped());
        if ready.is_some() {
            self.auth.lease.message("");
        }
        let mut child = command.spawn().map_err(native_spawn_error)?;
        let stdin = child.stdin.take().ok_or(SshError::ChannelClosed)?;
        let stdout = child.stdout.take().ok_or(SshError::ChannelClosed)?;
        let stderr = child.stderr.take().ok_or(SshError::ChannelClosed)?;
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let session_stop = self.stop.clone();
        let (exit_tx, exit) = watch::channel(None);
        let lease = self.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                biased;
                _=stopped.cancelled()=>{let _=child.kill().await; None},
                _=session_stop.cancelled()=>{let _=child.kill().await; None},
                status=child.wait()=>status.ok().and_then(|s|s.code()).and_then(|n|u32::try_from(n).ok()),
            };
            let _ = exit_tx.send(Some(status.unwrap_or(255)));
            drop(lease);
        });
        let mut process = NativeProcess {
            stdin: Some(stdin),
            stdout,
            stderr: Some(stderr),
            exit,
            stop,
        };
        if let Some((listener, token)) = ready {
            wait_for_authentication(
                listener,
                &token,
                &mut process,
                &self.stop,
                self.auth.lease.as_ref(),
            )
            .await?;
        }
        Ok(process)
    }
    pub async fn stream(
        self: &Arc<Self>,
        operation: &[&str],
        remote_command: Option<&str>,
    ) -> Result<SshStream, SshError> {
        let mut process = self.process(operation, remote_command).await?;
        if let Some(stderr) = process.stderr.take() {
            tokio::spawn(drain(stderr));
        }
        Ok(Box::new(process))
    }
    pub fn open_shell(
        self: &Arc<Self>,
        cols: u32,
        rows: u32,
        term: &str,
    ) -> Result<NativeShell, SshError> {
        let pair = native_pty_system()
            .openpty(size(cols, rows))
            .map_err(|e| SshError::Protocol(e.to_string()))?;
        let mut command = CommandBuilder::new(ssh_binary());
        command.args(["-o", "LogLevel=ERROR", "-tt"]);
        command.args(&self.args[..self.args.len() - 1]);
        command.arg(&self.args[self.args.len() - 1]);
        for (k, v) in &self.auth.environment {
            command.env(k, v);
        }
        command.env("SSH_ASKPASS", &self.auth.helper);
        command.env("SSH_ASKPASS_REQUIRE", "force");
        command.env("DISPLAY", "selai:0");
        command.env("TERM", term);
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| SshError::Protocol(e.to_string()))?;
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| SshError::Protocol(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| SshError::Protocol(e.to_string()))?;
        let killer = Arc::new(Mutex::new(child.clone_killer()));
        let stop = CancellationToken::new();
        let (events_tx, events) = mpsc::channel(64);
        let read_events = events_tx.clone();
        let read_stop = stop.clone();
        std::thread::spawn(move || {
            let mut bytes = [0; 32768];
            while !read_stop.is_cancelled() {
                match reader.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if read_events
                            .blocking_send(ShellEvent::Data(bytes[..n].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        let master = Arc::new(Mutex::new(Some(pair.master)));
        let wait_master = master.clone();
        let wait_stop = stop.clone();
        let lease = self.clone();
        std::thread::spawn(move || {
            let status = child.wait().ok().map(|s| s.exit_code()).unwrap_or(255);
            std::thread::sleep(Duration::from_millis(150));
            if let Ok(mut m) = wait_master.lock() {
                m.take();
            }
            let _ = events_tx.blocking_send(ShellEvent::Exit { status });
            let _ = events_tx.blocking_send(ShellEvent::Closed);
            wait_stop.cancel();
            drop(lease);
        });
        let session_stop = self.stop.clone();
        let kill_stop = stop.clone();
        tokio::spawn(async move {
            tokio::select! {_=session_stop.cancelled()=>{},_=kill_stop.cancelled()=>{}}
            if let Ok(mut k) = killer.lock() {
                let _ = k.kill();
            }
        });
        Ok(NativeShell {
            writer: Mutex::new(writer),
            master,
            events,
            stop,
        })
    }
    pub async fn remote_forward(
        self: &Arc<Self>,
        bind: &str,
        port: u16,
    ) -> Result<(u16, mpsc::Receiver<SshStream>), SshError> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let local_port = listener.local_addr()?.port();
        let bind_spec = if bind.contains(':') {
            format!("[{bind}]")
        } else {
            bind.into()
        };
        let spec = format!("{bind_spec}:{port}:127.0.0.1:{local_port}");
        let mut process = self
            .process(
                &[
                    "-v",
                    "-N",
                    "-o",
                    "ClearAllForwardings=no",
                    "-o",
                    "ExitOnForwardFailure=yes",
                    "-R",
                    &spec,
                ],
                None,
            )
            .await?;
        let stderr = process.stderr.take().ok_or(SshError::ChannelClosed)?;
        let mut lines = BufReader::new(stderr).lines();
        let ready = async {
            let mut actual = port;
            let mut accepted = false;
            while let Some(line) = lines.next_line().await? {
                if let Some(rest) = line.strip_prefix("Allocated port ") {
                    actual = rest
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                }
                if line.contains("remote forward success for:") {
                    accepted = true;
                }
                if accepted && actual != 0 {
                    return Ok::<u16, SshError>(actual);
                }
                if line.contains("remote port forwarding failed") {
                    return Err(SshError::Protocol(
                        "Server refused remote forwarding".into(),
                    ));
                }
            }
            Err(SshError::ChannelClosed)
        };
        let actual = tokio::time::timeout(Duration::from_secs(300), ready)
            .await
            .map_err(|_| SshError::ConnectTimeout)??;
        self.forwards
            .lock()
            .map_err(|_| SshError::ChannelClosed)?
            .insert((bind.into(), actual), process.stop.clone());
        let cancelled = self.stop.clone();
        let forwarded = process.stop.clone();
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(async move {
            let mut exit = process.exit.clone();
            loop {
                tokio::select! {biased;_=cancelled.cancelled()=>break,_=forwarded.cancelled()=>break,_=exit.changed()=>break,line=lines.next_line()=>if !matches!(line,Ok(Some(_))){break},_=tx.closed()=>break,accepted=listener.accept()=>{let Ok((socket,_))=accepted else {break}; if tx.try_send(Box::new(socket) as SshStream).is_err(){continue}}}
            }
            drop(process);
        });
        Ok((actual, rx))
    }
    pub fn cancel_remote_forward(&self, bind: &str, port: u16) {
        if let Ok(mut forwards) = self.forwards.lock() {
            if let Some(stop) = forwards.remove(&(bind.into(), port)) {
                stop.cancel();
            }
        }
    }
}

// tempfile uses a private 0700 directory on Unix. Windows temp directories may
// inherit additional ACL entries, so explicitly restrict this trust material.
async fn protect_pin_directory(path: &std::path::Path) -> Result<(), SshError> {
    #[cfg(windows)]
    {
        let system = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
            .join("System32");
        let mut who = Command::new(system.join("whoami.exe"));
        who.args(["/user", "/fo", "csv", "/nh"])
            .creation_flags(0x08000000)
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(10), who.output())
            .await
            .map_err(|_| SshError::ConnectTimeout)??;
        let output = String::from_utf8_lossy(&output.stdout);
        let sid = output
            .trim()
            .split(',')
            .nth(1)
            .unwrap_or_default()
            .trim_matches('"');
        if !sid.starts_with("S-1-")
            || !sid
                .bytes()
                .all(|c| c.is_ascii_digit() || matches!(c, b'S' | b'-'))
        {
            return Err(SshError::Protocol(
                "Cannot identify the Windows account".into(),
            ));
        }
        let mut acl = Command::new(system.join("icacls.exe"));
        acl.arg(path)
            .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)(F)")])
            .creation_flags(0x08000000)
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(10), acl.output())
            .await
            .map_err(|_| SshError::ConnectTimeout)??;
        if !output.status.success() {
            return Err(SshError::Protocol(
                "Cannot protect the temporary SSH host-key pins".into(),
            ));
        }
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

/// LocalCommand acknowledges authentication before SFTP/exec deadlines start.
/// Only our helper is permitted; arbitrary LocalCommand from ssh_config stays
/// disabled. The helper waits for an ACK before OpenSSH starts the operation.
async fn wait_for_authentication(
    listener: tokio::net::TcpListener,
    token: &str,
    process: &mut NativeProcess,
    cancel: &CancellationToken,
    interaction: &dyn NativeInteraction,
) -> Result<(), SshError> {
    use tokio::io::AsyncWriteExt;
    let deadline = tokio::time::sleep(Duration::from_secs(300));
    tokio::pin!(deadline);
    let mut errors = Vec::new();
    let mut buffer = [0; 4096];
    let mut stderr_open = true;
    loop {
        tokio::select! {
            biased;
            _=cancel.cancelled()=>return Err(SshError::ChannelClosed),
            _=&mut deadline=>return Err(SshError::ConnectTimeout),
            accepted=listener.accept()=>{
                let(mut socket,_)=accepted?;let mut supplied=vec![0;token.len()];
                if !matches!(tokio::time::timeout(Duration::from_secs(5),socket.read_exact(&mut supplied)).await,Ok(Ok(_))){continue;}
                if supplied==token.as_bytes(){socket.write_all(&[1]).await?;return Ok(());}
            },
            _=process.exit.changed()=>return Err(SshError::NativeAuth(String::from_utf8_lossy(&errors).into_owned())),
            read=async{match process.stderr.as_mut(){Some(stderr)=>stderr.read(&mut buffer).await,None=>Ok(0)}},if stderr_open=>match read{
                Ok(0)|Err(_)=>stderr_open=false,
                Ok(n)=>{interaction.message(&String::from_utf8_lossy(&buffer[..n]));let count=n.min(8192usize.saturating_sub(errors.len()));errors.extend_from_slice(&buffer[..count]);},
            },
        }
    }
}

fn native_spawn_error(error: std::io::Error) -> SshError {
    SshError::Protocol(format!("System OpenSSH could not start. Install the OpenSSH client for this operating system: {error}"))
}
async fn drain(mut stderr: ChildStderr) {
    let mut bytes = [0; 4096];
    while matches!(stderr.read(&mut bytes).await, Ok(1..)) {}
}

pub(crate) struct NativeProcess {
    pub stdin: Option<ChildStdin>,
    pub stdout: ChildStdout,
    pub stderr: Option<ChildStderr>,
    pub exit: watch::Receiver<Option<u32>>,
    pub stop: CancellationToken,
}
impl Drop for NativeProcess {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl AsyncRead for NativeProcess {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}
impl AsyncWrite for NativeProcess {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.stdin.as_mut() {
            Some(stdin) => Pin::new(stdin).poll_write(cx, buf),
            None => Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into())),
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.stdin.as_mut() {
            Some(stdin) => Pin::new(stdin).poll_flush(cx),
            None => Poll::Ready(Ok(())),
        }
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.stdin.as_mut() {
            Some(stdin) => Pin::new(stdin).poll_shutdown(cx),
            None => Poll::Ready(Ok(())),
        }
    }
}

fn size(cols: u32, rows: u32) -> PtySize {
    PtySize {
        cols: cols.min(u16::MAX.into()) as u16,
        rows: rows.min(u16::MAX.into()) as u16,
        pixel_width: 0,
        pixel_height: 0,
    }
}
pub(crate) struct NativeShell {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Arc<Mutex<Option<Box<dyn portable_pty::MasterPty + Send>>>>,
    events: mpsc::Receiver<ShellEvent>,
    stop: CancellationToken,
}
impl Drop for NativeShell {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl NativeShell {
    pub fn write(&self, bytes: &[u8]) -> Result<(), SshError> {
        let mut writer = self.writer.lock().map_err(|_| SshError::ChannelClosed)?;
        writer.write_all(bytes)?;
        writer.flush()?;
        Ok(())
    }
    pub fn resize(&self, cols: u32, rows: u32) -> Result<(), SshError> {
        if let Some(master) = self
            .master
            .lock()
            .map_err(|_| SshError::ChannelClosed)?
            .as_ref()
        {
            master
                .resize(size(cols, rows))
                .map_err(|e| SshError::Protocol(e.to_string()))?;
        }
        Ok(())
    }
    pub async fn next_event(&mut self) -> ShellEvent {
        self.events.recv().await.unwrap_or(ShellEvent::Closed)
    }
    pub fn close(&self) {
        self.stop.cancel();
    }
}
