//! OpenSSH askpass IPC. Responses exist only in memory and helper stdout;
//! neither command lines nor environment variables contain credentials.
use crate::{dto::CommandError, service::Services};
use selai_domain::{AuthMethod, ErrorCode, HostId};
use selai_ssh::NativeAuth;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{mpsc, Arc, Mutex, Weak},
    time::Duration,
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Challenge {
    pub id: String,
    pub host_id: String,
    pub host_label: String,
    pub prompt: String,
    pub instructions: String,
    pub kind: String,
}
pub type Sink = Arc<dyn Fn(Challenge) + Send + Sync>;
struct Registration {
    host_id: String,
    label: String,
    cancel: tokio_util::sync::CancellationToken,
    instructions: String,
}
struct Pending {
    token: String,
    challenge: Challenge,
    reply: mpsc::Sender<Option<Zeroizing<String>>>,
}
#[derive(Default)]
struct Inner {
    registrations: HashMap<String, Registration>,
    pending: HashMap<String, Pending>,
    sink: Option<Sink>,
    address: Option<String>,
}
#[derive(Default)]
pub(crate) struct Registry {
    inner: Arc<Mutex<Inner>>,
}
struct Lease {
    inner: Weak<Mutex<Inner>>,
    token: String,
}
impl selai_ssh::NativeInteraction for Lease {
    fn message(&self, text: &str) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut inner) = inner.lock() else { return };
        let Some(reg) = inner.registrations.get_mut(&self.token) else {
            return;
        };
        if text.is_empty() {
            reg.instructions.clear();
        } else {
            let remaining = 8192usize.saturating_sub(reg.instructions.len());
            reg.instructions.extend(text.chars().take(remaining / 4));
        }
        let instructions = reg.instructions.clone();
        let sink = inner.sink.clone();
        for pending in inner.pending.values_mut().filter(|p| p.token == self.token) {
            pending.challenge.instructions = instructions.clone();
            if let Some(sink) = &sink {
                sink(pending.challenge.clone());
            }
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            if let Ok(mut inner) = inner.lock() {
                inner.registrations.remove(&self.token);
                let ids = inner
                    .pending
                    .iter()
                    .filter(|(_, pending)| pending.token == self.token)
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                for id in ids {
                    if let Some(pending) = inner.pending.remove(&id) {
                        if let Some(sink) = &inner.sink {
                            let mut dismissed = pending.challenge;
                            dismissed.kind = "dismiss".into();
                            sink(dismissed);
                        }
                    }
                }
            }
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    prompt: String,
    kind: String,
}
#[derive(Serialize, Deserialize)]
struct Reply {
    answer: Option<String>,
}
impl Drop for Reply {
    fn drop(&mut self) {
        if let Some(answer) = &mut self.answer {
            answer.zeroize();
        }
    }
}
fn error(message: impl Into<String>) -> CommandError {
    CommandError::new(ErrorCode::SshAuthFailed, message)
}
impl Registry {
    pub fn watch(&self, sink: Sink) -> Result<Vec<Challenge>, CommandError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| error("Authentication state unavailable"))?;
        inner.sink = Some(sink);
        Ok(inner
            .pending
            .values()
            .map(|p| p.challenge.clone())
            .collect())
    }
    pub fn answer(&self, id: &str, answer: Option<String>) -> Result<(), CommandError> {
        let answer = answer.map(Zeroizing::new);
        if answer
            .as_ref()
            .is_some_and(|a| a.len() > 8192 || a.contains(['\r', '\n', '\0']))
        {
            return Err(error("Authentication response is invalid"));
        }
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| error("Authentication state unavailable"))?;
        let pending = inner
            .pending
            .remove(id)
            .ok_or_else(|| error("Authentication request expired"))?;
        let registration = inner
            .registrations
            .get(&pending.token)
            .ok_or_else(|| error("Authentication request expired"))?;
        if registration.cancel.is_cancelled() {
            return Err(error("The vault is locking"));
        }
        let cancelled = answer.is_none();
        pending
            .reply
            .send(answer)
            .map_err(|_| error("Authentication request expired"))?;
        if cancelled {
            registration.cancel.cancel();
        }
        if let Some(sink) = &inner.sink {
            let mut dismissed = pending.challenge;
            dismissed.kind = "dismiss".into();
            sink(dismissed);
        }
        Ok(())
    }
    pub fn register(
        &self,
        host_id: String,
        label: String,
        method: AuthMethod,
        identity: Option<String>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<NativeAuth, CommandError> {
        let cancel = cancel.child_token();
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| error("Authentication state unavailable"))?;
        if inner.registrations.len() >= 64 {
            return Err(error("Too many SSH authentication sessions"));
        }
        let address = match &inner.address {
            Some(address) => address.clone(),
            None => {
                let listener =
                    TcpListener::bind("127.0.0.1:0").map_err(|e| error(e.to_string()))?;
                listener
                    .set_nonblocking(true)
                    .map_err(|e| error(e.to_string()))?;
                let address = listener
                    .local_addr()
                    .map_err(|e| error(e.to_string()))?
                    .to_string();
                let weak = Arc::downgrade(&self.inner);
                std::thread::spawn(move || {
                    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                    while weak.strong_count() > 0 {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                if active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 16 {
                                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                                    continue;
                                }
                                let weak = weak.clone();
                                let active = active.clone();
                                std::thread::spawn(move || {
                                    let _ = serve(stream, weak);
                                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                                });
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(50))
                            }
                            Err(_) => break,
                        }
                    }
                });
                inner.address = Some(address.clone());
                address
            }
        };
        let token = HostId::new().to_string();
        inner.registrations.insert(
            token.clone(),
            Registration {
                host_id,
                label,
                cancel: cancel.clone(),
                instructions: String::new(),
            },
        );
        let lease = Arc::new(Lease {
            inner: Arc::downgrade(&self.inner),
            token: token.clone(),
        });
        Ok(NativeAuth {
            method: method.as_str().into(),
            identity: identity.map(Into::into),
            helper: std::env::current_exe().map_err(|e| error(e.to_string()))?,
            environment: vec![
                ("SELAI_SSH_AUTH_ADDR".into(), address),
                ("SELAI_SSH_AUTH_TOKEN".into(), token),
            ],
            lease,
            cancel,
        })
    }
}
fn serve(
    mut stream: TcpStream,
    inner: Weak<Mutex<Inner>>,
) -> Result<(), Box<dyn std::error::Error>> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut line = String::new();
    BufReader::new((&stream).take(16384)).read_line(&mut line)?;
    let request: Request = serde_json::from_str(&line)?;
    let Some(inner) = inner.upgrade() else {
        return Ok(());
    };
    let (reply, rx) = mpsc::channel();
    let id = HostId::new().to_string();
    let (sink, challenge, cancel) = {
        let mut inner = inner.lock().map_err(|_| "State unavailable")?;
        let Some(reg) = inner.registrations.get(&request.token) else {
            return Ok(());
        };
        if reg.cancel.is_cancelled() {
            return Ok(());
        }
        let challenge = Challenge {
            id: id.clone(),
            host_id: reg.host_id.clone(),
            host_label: reg.label.clone(),
            prompt: request.prompt.chars().take(4096).collect(),
            instructions: reg.instructions.clone(),
            kind: request.kind,
        };
        let cancel = reg.cancel.clone();
        let sink = inner.sink.clone();
        if sink.is_none() {
            return Ok(());
        }
        inner.pending.insert(
            id.clone(),
            Pending {
                token: request.token,
                challenge: challenge.clone(),
                reply,
            },
        );
        (sink, challenge, cancel)
    };
    if let Some(sink) = sink {
        sink(challenge);
    }
    let started = std::time::Instant::now();
    stream.set_nonblocking(true)?;
    let answer = loop {
        if cancel.is_cancelled() || started.elapsed() > Duration::from_secs(300) {
            break None;
        }
        match stream.peek(&mut [0; 1]) {
            Ok(0) => break None,
            Err(error) if error.kind() != std::io::ErrorKind::WouldBlock => break None,
            _ => {}
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(answer) => break answer,
            Err(mpsc::RecvTimeoutError::Disconnected) => break None,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    if let Ok(mut inner) = inner.lock() {
        if let Some(pending) = inner.pending.remove(&id) {
            if let Some(sink) = &inner.sink {
                let mut dismissed = pending.challenge;
                dismissed.kind = "dismiss".into();
                sink(dismissed);
            }
        }
    }
    stream.set_nonblocking(false)?;
    let mut response = Zeroizing::new(serde_json::to_string(&Reply {
        answer: answer.as_ref().map(|s| s.to_string()),
    })?);
    response.push('\n');
    stream.write_all(response.as_bytes())?;
    Ok(())
}

impl Services {
    pub fn ssh_auth_watch(&self, sink: Sink) -> Result<Vec<Challenge>, CommandError> {
        self.ssh_auth.watch(sink)
    }
    pub fn ssh_auth_answer(&self, id: &str, answer: Option<String>) -> Result<(), CommandError> {
        self.ssh_auth.answer(id, answer)
    }
}

/// Called before constructing Tauri. OpenSSH invokes this executable as its
/// askpass/proxy helper; a helper never opens a second UI or reads the vault.
pub fn helper_main() -> Option<i32> {
    let proxy = std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--selai-ssh-proxy");
    let ready = std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--selai-ssh-ready");
    let askpass = std::env::var_os("SSH_ASKPASS_REQUIRE").is_some_and(|v| v == "force")
        && std::env::var_os("SELAI_SSH_AUTH_ADDR").is_some();
    if !proxy && !ready && !askpass {
        return None;
    }
    Some(
        if (if proxy {
            proxy_helper()
        } else if ready {
            ready_helper()
        } else {
            askpass_helper()
        })
        .is_ok()
        {
            0
        } else {
            1
        },
    )
}
fn local_stream(name: &str) -> Result<TcpStream, Box<dyn std::error::Error>> {
    let address: std::net::SocketAddr = std::env::var(name)?.parse()?;
    if !address.ip().is_loopback() {
        return Err("Invalid helper endpoint".into());
    }
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(310)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    Ok(stream)
}
fn askpass_helper() -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = local_stream("SELAI_SSH_AUTH_ADDR")?;
    let request = Request {
        token: std::env::var("SELAI_SSH_AUTH_TOKEN")?,
        prompt: std::env::args().nth(1).unwrap_or_default(),
        kind: std::env::var("SSH_ASKPASS_PROMPT").unwrap_or_else(|_| "secret".into()),
    };
    let line = serde_json::to_string(&request)?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut line = Zeroizing::new(String::new());
    BufReader::new(stream.take(16384)).read_line(&mut line)?;
    let mut response: Reply = serde_json::from_str(&line)?;
    let answer = Zeroizing::new(response.answer.take().ok_or("Authentication cancelled")?);
    std::io::stdout().write_all(answer.as_bytes())?;
    std::io::stdout().write_all(b"\n")?;
    Ok(())
}
fn ready_helper() -> Result<(), Box<dyn std::error::Error>> {
    let mut socket = local_stream("SELAI_SSH_READY_ADDR")?;
    socket.write_all(std::env::var("SELAI_SSH_READY_TOKEN")?.as_bytes())?;
    let mut ack = [0; 1];
    socket.read_exact(&mut ack)?;
    if ack != [1] {
        return Err("Authentication readiness was not acknowledged".into());
    }
    Ok(())
}

fn proxy_helper() -> Result<(), Box<dyn std::error::Error>> {
    let mut socket = local_stream("SELAI_SSH_PROXY_ADDR")?;
    socket.set_read_timeout(None)?;
    socket.write_all(std::env::var("SELAI_SSH_AUTH_TOKEN")?.as_bytes())?;
    let mut input = socket.try_clone()?;
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin(), &mut input);
        let _ = input.shutdown(std::net::Shutdown::Write);
    });
    std::io::copy(&mut socket, &mut std::io::stdout())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    fn setup() -> (
        Arc<Registry>,
        NativeAuth,
        mpsc::Receiver<Challenge>,
        tokio_util::sync::CancellationToken,
    ) {
        let registry = Arc::new(Registry::default());
        let (tx, rx) = mpsc::channel();
        registry
            .watch(Arc::new(move |c| {
                let _ = tx.send(c);
            }))
            .unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let auth = registry
            .register(
                "host-test".into(),
                "Test server".into(),
                AuthMethod::KeyboardInteractive,
                None,
                cancel.clone(),
            )
            .unwrap();
        (registry, auth, rx, cancel)
    }
    fn request(auth: &NativeAuth, token: Option<&str>) -> std::thread::JoinHandle<Option<String>> {
        let addr = auth
            .environment
            .iter()
            .find(|(k, _)| k == "SELAI_SSH_AUTH_ADDR")
            .unwrap()
            .1
            .clone();
        let token = token.map(str::to_owned).unwrap_or_else(|| {
            auth.environment
                .iter()
                .find(|(k, _)| k == "SELAI_SSH_AUTH_TOKEN")
                .unwrap()
                .1
                .clone()
        });
        std::thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let line = serde_json::to_string(&Request {
                token,
                prompt: "Verification code:".into(),
                kind: "secret".into(),
            })
            .unwrap();
            writeln!(stream, "{line}").unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            if line.is_empty() {
                None
            } else {
                serde_json::from_str::<Reply>(&line).unwrap().answer.take()
            }
        })
    }
    #[test]
    fn response_is_bound_to_host_and_single_use() {
        let (registry, auth, events, _) = setup();
        auth.lease
            .message("Choose a code from your authenticator app.\n");
        let helper = request(&auth, None);
        let challenge = events.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(challenge.instructions.contains("authenticator app"));
        assert_eq!(challenge.host_id, "host-test");
        assert_eq!(challenge.prompt, "Verification code:");
        registry
            .answer(&challenge.id, Some("123456".into()))
            .unwrap();
        assert_eq!(helper.join().unwrap().as_deref(), Some("123456"));
        assert!(registry
            .answer(&challenge.id, Some("again".into()))
            .is_err());
        assert!(registry.inner.lock().unwrap().pending.is_empty());
    }
    #[test]
    fn unknown_capability_cannot_open_a_prompt() {
        let (registry, auth, events, _) = setup();
        assert!(request(&auth, Some("invalid-token"))
            .join()
            .unwrap()
            .is_none());
        assert!(events.try_recv().is_err());
        assert!(registry.inner.lock().unwrap().pending.is_empty());
    }
    #[test]
    fn vault_lock_cancels_pending_authentication() {
        let (registry, auth, events, cancel) = setup();
        let helper = request(&auth, None);
        let challenge = events.recv_timeout(Duration::from_secs(3)).unwrap();
        cancel.cancel();
        assert!(helper.join().unwrap().is_none());
        assert!(registry.answer(&challenge.id, Some("late".into())).is_err());
    }
    #[test]
    fn dropping_session_revokes_askpass_capability() {
        let (registry, auth, events, _) = setup();
        let helper = request(&auth, None);
        let challenge = events.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(auth);
        assert!(helper.join().unwrap().is_none());
        assert!(registry.answer(&challenge.id, None).is_err());
        assert!(registry.inner.lock().unwrap().registrations.is_empty());
    }
    #[test]
    fn invalid_response_does_not_consume_the_challenge() {
        let (registry, auth, events, parent) = setup();
        let helper = request(&auth, None);
        let challenge = events.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(registry
            .answer(&challenge.id, Some("bad\nresponse".into()))
            .is_err());
        registry.answer(&challenge.id, None).unwrap();
        assert!(auth.cancel.is_cancelled());
        assert!(!parent.is_cancelled());
        assert!(helper.join().unwrap().is_none());
    }
    #[test]
    fn closing_a_hardware_notification_helper_dismisses_the_prompt() {
        let (registry, auth, events, _) = setup();
        let addr = &auth
            .environment
            .iter()
            .find(|(k, _)| k == "SELAI_SSH_AUTH_ADDR")
            .unwrap()
            .1;
        let token = auth
            .environment
            .iter()
            .find(|(k, _)| k == "SELAI_SSH_AUTH_TOKEN")
            .unwrap()
            .1
            .clone();
        let mut socket = TcpStream::connect(addr).unwrap();
        writeln!(
            socket,
            "{}",
            serde_json::to_string(&Request {
                token,
                prompt: "Touch your security key".into(),
                kind: "none".into()
            })
            .unwrap()
        )
        .unwrap();
        let challenge = events.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(socket);
        let dismissed = events.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(dismissed.id, challenge.id);
        assert_eq!(dismissed.kind, "dismiss");
        assert!(registry.answer(&challenge.id, None).is_err());
    }
}
