//! Saved proxy/tunnel profiles and the lifetime of network operations.
use crate::{
    dto::CommandError,
    service::{now_ms, ssh_error, Services},
};
use selai_domain::{ErrorCode, HostId, Hostname};
use selai_secrets::{SecretKey, SecretValue};
use selai_ssh::{
    ConnectOptions, Endpoint, ForwardCounters, ForwardSpec, Forwarder, ProxyKind, ProxyOptions,
    Session,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub(crate) fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(ErrorCode::InvalidInput, message)
}
pub(crate) fn failed(error: impl std::fmt::Display) -> CommandError {
    CommandError::new(ErrorCode::StorageIo, error.to_string())
}
pub(crate) fn state_lock<T>(lock: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, CommandError> {
    lock.lock()
        .map_err(|_| CommandError::new(ErrorCode::Internal, "Network state is unavailable."))
}
fn parse_id(id: &str) -> Result<HostId, CommandError> {
    id.parse().map_err(|_| invalid("Invalid profile ID."))
}
fn name(value: &str) -> Result<String, CommandError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(invalid("Name must be 1 to 128 characters."));
    }
    Ok(value.into())
}
fn hostname(value: &str) -> Result<String, CommandError> {
    Hostname::parse(value)
        .map(|h| h.as_str().into())
        .map_err(|_| invalid("Enter a valid hostname or IP address."))
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub hostname: String,
    pub port: u16,
    pub username: Option<String>,
    pub password_configured: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyInput {
    pub id: Option<String>,
    pub name: String,
    pub kind: String,
    pub hostname: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default)]
    pub clear_password: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardProfile {
    pub id: String,
    pub name: String,
    pub host_id: String,
    pub mode: String,
    pub bind_address: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardView {
    pub profile: ForwardProfile,
    pub status: String,
    pub actual_port: u16,
    pub connections: u64,
    pub sent: u64,
    pub received: u64,
    pub connection_errors: u64,
    pub error: Option<String>,
}
pub type ForwardSink = Arc<dyn Fn(ForwardView) + Send + Sync>;
pub(crate) struct ForwardLive {
    pub stop: CancellationToken,
    pub view: Arc<Mutex<ForwardView>>,
    event: Arc<Mutex<ForwardSink>>,
}
impl ForwardLive {
    fn active(&self) -> bool {
        self.view
            .lock()
            .map(|v| ["connecting", "running"].contains(&v.status.as_str()))
            .unwrap_or(true)
    }
}
pub(crate) struct NetworkRegistry {
    pub sftp: Mutex<HashMap<String, Arc<crate::sftp_service::SftpConnection>>>,
    pub transfers: Mutex<HashMap<String, Arc<crate::sftp_service::TransferJob>>>,
    pub tunnels: Mutex<HashMap<String, ForwardLive>>,
    pub transfer_slots: tokio::sync::Semaphore,
}
impl Default for NetworkRegistry {
    fn default() -> Self {
        Self {
            sftp: Mutex::new(HashMap::new()),
            transfers: Mutex::new(HashMap::new()),
            tunnels: Mutex::new(HashMap::new()),
            transfer_slots: tokio::sync::Semaphore::new(2),
        }
    }
}
impl NetworkRegistry {
    pub(crate) fn stop_all(&self) {
        if let Ok(tunnels) = self.tunnels.lock() {
            for tunnel in tunnels.values() {
                tunnel.stop.cancel();
            }
        }
        if let Ok(transfers) = self.transfers.lock() {
            for transfer in transfers.values() {
                transfer.stop.cancel();
            }
        }
        if let Ok(sessions) = self.sftp.lock() {
            for session in sessions.values() {
                session.stop.cancel();
            }
        }
    }
}

impl Services {
    pub fn proxy_list(&self) -> Result<Vec<ProxyView>, CommandError> {
        self.lock_db_pub()?
            .network_profiles("proxy")
            .map_err(failed)?
            .iter()
            .map(|j| serde_json::from_str(j).map_err(failed))
            .collect()
    }
    pub fn proxy_save(&self, input: ProxyInput) -> Result<ProxyView, CommandError> {
        if !["http", "socks5"].contains(&input.kind.as_str()) || input.port == 0 {
            return Err(invalid(
                "Choose HTTP CONNECT or SOCKS5 and a port from 1 to 65535.",
            ));
        }
        let id = input
            .id
            .as_deref()
            .map(parse_id)
            .transpose()?
            .unwrap_or_else(HostId::new);
        let key = SecretKey::proxy_password(id);
        let username = input.username.filter(|s| !s.is_empty());
        if username
            .as_ref()
            .is_some_and(|s| s.len() > 255 || s.contains(':') || s.chars().any(char::is_control))
        {
            return Err(invalid(
                "Proxy username must be at most 255 bytes, without colon or control characters.",
            ));
        }
        let mut view = ProxyView {
            id: id.to_string(),
            name: name(&input.name)?,
            kind: input.kind,
            hostname: hostname(&input.hostname)?,
            port: input.port,
            username,
            password_configured: false,
        };
        let password = zeroize::Zeroizing::new(input.password.unwrap_or_default());
        if password.len() > 255 {
            return Err(invalid("Proxy password must be at most 255 bytes."));
        }
        if input.clear_password || view.username.is_none() {
            self.secrets.delete(&key).map_err(failed)?;
        } else if !password.is_empty() {
            self.secrets
                .put(&key, &SecretValue::new(password.as_bytes().to_vec()))
                .map_err(failed)?;
        }
        view.password_configured = self.secrets.get(&key).map_err(failed)?.is_some();
        self.lock_db_pub()?
            .save_network_profile(
                &view.id,
                "proxy",
                &serde_json::to_string(&view).map_err(failed)?,
                now_ms(),
            )
            .map_err(failed)?;
        Ok(view)
    }
    pub fn proxy_delete(&self, id: &str) -> Result<(), CommandError> {
        let key = SecretKey::proxy_password(parse_id(id)?);
        let db = self.lock_db_pub()?;
        if db.proxy_usage_count(id).map_err(failed)? > 0 {
            return Err(invalid("This proxy is assigned to SSH profiles. Disable it in those profiles before deleting it."));
        }
        db.delete_network_profile(id, "proxy").map_err(failed)?;
        drop(db);
        self.secrets.delete(&key).map_err(failed)
    }
    pub(crate) fn proxy_for_host(&self, id: HostId) -> Result<Option<ProxyOptions>, CommandError> {
        let db = self.lock_db_pub()?;
        let Some(proxy_id) = db.host_proxy(&id.to_string()).map_err(failed)? else {
            return Ok(None);
        };
        let json = db
            .network_profile(&proxy_id, "proxy")
            .map_err(failed)?
            .ok_or_else(|| invalid("The assigned proxy no longer exists."))?;
        drop(db);
        let p: ProxyView = serde_json::from_str(&json).map_err(failed)?;
        let password = self
            .secrets
            .get(&SecretKey::proxy_password(parse_id(&p.id)?))
            .map_err(failed)?;
        Ok(Some(ProxyOptions {
            endpoint: Endpoint::new(p.hostname, p.port),
            kind: if p.kind == "http" {
                ProxyKind::Http
            } else {
                ProxyKind::Socks5
            },
            username: p.username,
            password,
        }))
    }
    pub(crate) async fn network_connect(
        &self,
        host_id: &str,
    ) -> Result<Arc<Session>, CommandError> {
        let id = parse_id(host_id)?;
        let (host, pins) = self.active_host_and_pins(id)?;
        if host.is_local {
            return Err(invalid("Choose an SSH server."));
        }
        let proxy = self.proxy_for_host(id)?;
        let auth = self.load_auth(id, &host)?;
        let options = ConnectOptions {
            endpoint: Endpoint::new(host.hostname, host.port),
            username: host.username.clone(),
            auth,
            pinned: pins,
            connect_timeout: Duration::from_secs(host.connect_timeout_seconds.into()),
            keepalive: Some(Duration::from_secs(30)),
        };
        let session = tokio::select! {biased;_=self.agent.drain.cancelled()=>return Err(invalid("The vault is locking.")),result=selai_ssh::connect_via(options,proxy)=>result};
        match session {
            Ok(session) => Ok(Arc::new(session)),
            Err(e) => {
                self.remember_observed_key(id, &e);
                Err(ssh_error(&e))
            }
        }
    }
    pub fn forward_watch(&self, sink: ForwardSink) -> Result<Vec<ForwardView>, CommandError> {
        {
            let live = state_lock(&self.network.tunnels)?;
            for runtime in live.values() {
                *state_lock(&runtime.event)? = sink.clone();
            }
        }
        self.forward_list()
    }
    pub fn forward_list(&self) -> Result<Vec<ForwardView>, CommandError> {
        let profiles: Vec<ForwardProfile> = self
            .lock_db_pub()?
            .network_profiles("forward")
            .map_err(failed)?
            .iter()
            .map(|j| serde_json::from_str(j).map_err(failed))
            .collect::<Result<_, _>>()?;
        let live = state_lock(&self.network.tunnels)?;
        profiles
            .into_iter()
            .map(|profile| {
                if let Some(runtime) = live.get(&profile.id) {
                    Ok(state_lock(&runtime.view)?.clone())
                } else {
                    Ok(stopped(profile))
                }
            })
            .collect()
    }
    pub fn forward_save(&self, mut profile: ForwardProfile) -> Result<ForwardView, CommandError> {
        if profile.id.is_empty() {
            profile.id = HostId::new().to_string();
        }
        parse_id(&profile.id)?;
        if let Some(live) = state_lock(&self.network.tunnels)?.get(&profile.id) {
            if live.active() {
                return Err(invalid("Stop the tunnel before editing it."));
            }
        }
        profile.name = name(&profile.name)?;
        let host = parse_id(&profile.host_id)?;
        let (h, _) = self.active_host_and_pins(host)?;
        if h.is_local {
            return Err(invalid("Choose an SSH server."));
        }
        if !["local", "remote", "dynamic"].contains(&profile.mode.as_str()) {
            return Err(invalid("Choose a forwarding mode."));
        }
        profile.bind_address = hostname(&profile.bind_address)?;
        if profile.mode != "dynamic" {
            profile.target_host = hostname(&profile.target_host)?;
            if profile.target_port == 0 {
                return Err(invalid("Destination port must be 1 to 65535."));
            }
        }
        self.lock_db_pub()?
            .save_network_profile(
                &profile.id,
                "forward",
                &serde_json::to_string(&profile).map_err(failed)?,
                now_ms(),
            )
            .map_err(failed)?;
        state_lock(&self.network.tunnels)?.remove(&profile.id);
        Ok(stopped(profile))
    }
    pub fn forward_delete(&self, id: &str) -> Result<(), CommandError> {
        parse_id(id)?;
        if state_lock(&self.network.tunnels)?
            .get(id)
            .is_some_and(ForwardLive::active)
        {
            return Err(invalid("Stop the tunnel before deleting it."));
        }
        state_lock(&self.network.tunnels)?.remove(id);
        self.lock_db_pub()?
            .delete_network_profile(id, "forward")
            .map_err(failed)
    }
    pub fn forward_stop(&self, id: &str) -> Result<(), CommandError> {
        parse_id(id)?;
        if let Some(live) = state_lock(&self.network.tunnels)?.get(id) {
            live.stop.cancel();
        }
        Ok(())
    }
    pub async fn forward_start(
        &self,
        id: &str,
        on_event: Arc<dyn Fn(ForwardView) + Send + Sync>,
    ) -> Result<ForwardView, CommandError> {
        let json = self
            .lock_db_pub()?
            .network_profile(id, "forward")
            .map_err(failed)?
            .ok_or_else(|| invalid("Tunnel profile not found."))?;
        let profile: ForwardProfile = serde_json::from_str(&json).map_err(failed)?;
        let event = Arc::new(Mutex::new(on_event));
        let relay = event.clone();
        let on_event: ForwardSink = Arc::new(move |view| {
            if let Ok(sink) = relay.lock() {
                sink(view);
            }
        });
        // Keep the established listener and SSH tunnel alive through a vault
        // lock. The shared registry stops it when the application exits.
        let stop = CancellationToken::new();
        let view = Arc::new(Mutex::new(stopped(profile.clone())));
        {
            let mut registry = state_lock(&self.network.tunnels)?;
            if registry.get(id).is_some_and(ForwardLive::active) {
                return Err(invalid("This tunnel is already starting or running."));
            }
            if registry.values().filter(|l| l.active()).count() >= 16 {
                return Err(invalid("Up to 16 tunnels can run at once."));
            }
            state_lock(&view)?.status = "connecting".into();
            registry.insert(
                id.into(),
                ForwardLive {
                    stop: stop.clone(),
                    view: view.clone(),
                    event,
                },
            );
        }
        on_event(state_lock(&view)?.clone());
        let result = tokio::select! {biased;_=stop.cancelled()=>Err(invalid("Tunnel start cancelled.")),result=async {let session=self.network_connect(&profile.host_id).await?;let spec=ForwardSpec{mode:profile.mode.clone(),bind:profile.bind_address.clone(),port:profile.bind_port,target:profile.target_host.clone(),target_port:profile.target_port};tokio::time::timeout(Duration::from_secs(20),Forwarder::start(session,spec)).await.map_err(|_|invalid("Tunnel setup timed out."))?.map_err(|e|ssh_error(&e))}=>result};
        let worker = match result {
            Ok(worker) => worker,
            Err(e) => {
                stop.cancel();
                let mut v = state_lock(&view)?;
                v.status = "failed".into();
                v.error = Some(e.message.clone());
                on_event(v.clone());
                return Err(e);
            }
        };
        {
            let mut v = state_lock(&view)?;
            v.actual_port = worker.port();
            v.status = "running".into();
            on_event(v.clone());
        }
        let initial = state_lock(&view)?.clone();
        tokio::spawn(async move {
            let counters = Arc::new(ForwardCounters::default());
            let task = worker.run(stop.clone(), counters.clone());
            tokio::pin!(task);
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            let end = loop {
                tokio::select! {result=&mut task=>break result,_=interval.tick()=>{if let Ok(mut v)=view.lock(){let current=(counters.connections.load(Ordering::Relaxed),counters.sent.load(Ordering::Relaxed),counters.received.load(Ordering::Relaxed),counters.errors.load(Ordering::Relaxed));if current!=(v.connections,v.sent,v.received,v.connection_errors){(v.connections,v.sent,v.received,v.connection_errors)=current;on_event(v.clone());}}}}
            };
            stop.cancel();
            if let Ok(mut v) = view.lock() {
                v.connections = 0;
                v.sent = counters.sent.load(Ordering::Relaxed);
                v.received = counters.received.load(Ordering::Relaxed);
                v.connection_errors = counters.errors.load(Ordering::Relaxed);
                v.status = if end.is_ok() { "stopped" } else { "failed" }.into();
                v.error = end.err().map(|e| e.to_string());
                on_event(v.clone());
            }
        });
        Ok(initial)
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) async fn close_network(&self) {
        self.network.stop_all();
        if let Ok(tunnels) = self.network.tunnels.lock() {
            for t in tunnels.values() {
                t.stop.cancel();
            }
        }
        if let Ok(transfers) = self.network.transfers.lock() {
            for t in transfers.values() {
                t.stop.cancel();
            }
        }
        let sessions: Vec<_> = self
            .network
            .sftp
            .lock()
            .map(|mut m| m.drain().map(|(_, s)| s).collect())
            .unwrap_or_default();
        for session in sessions {
            session.stop.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(2), session.session.close()).await;
        }
    }
    pub(crate) fn network_active_count(&self) -> usize {
        let tunnels = self
            .network
            .tunnels
            .lock()
            .map(|m| {
                m.values()
                    .filter(|t| {
                        t.view
                            .lock()
                            .map(|v| ["connecting", "running"].contains(&v.status.as_str()))
                            .unwrap_or(true)
                    })
                    .count()
            })
            .unwrap_or(1);
        let transfers = self
            .network
            .transfers
            .lock()
            .map(|m| {
                m.values()
                    .filter(|t| {
                        t.view
                            .lock()
                            .map(|v| {
                                ["queued", "scanning", "transferring"].contains(&v.status.as_str())
                            })
                            .unwrap_or(true)
                    })
                    .count()
            })
            .unwrap_or(1);
        tunnels + transfers
    }
}
fn stopped(profile: ForwardProfile) -> ForwardView {
    ForwardView {
        actual_port: profile.bind_port,
        profile,
        status: "stopped".into(),
        connections: 0,
        sent: 0,
        received: 0,
        connection_errors: 0,
        error: None,
    }
}

#[cfg(all(test, target_os = "linux"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tokio::{io::AsyncReadExt, net::TcpStream};
    #[tokio::test]
    async fn vault_drain_closes_listeners_sftp_and_queued_transfers() {
        let server = selai_testkit::Sshd::start();
        let services = Services::in_memory(Arc::new(selai_secrets::MemoryStore::new())).unwrap();
        let input=serde_json::from_value(serde_json::json!({"label":"Network lock fixture","hostname":"127.0.0.1","port":server.port,"username":selai_testkit::USER,"authMethod":"password","password":selai_testkit::material().password,"connectTimeoutSeconds":10,"keepAliveSeconds":30,"terminalType":"xterm-256color"})).unwrap();
        let host = services.host_save(input).unwrap();
        let probe = services.host_probe(&host.id).await.unwrap();
        services
            .host_trust(
                &host.id,
                &probe.observed.fingerprint_sha256,
                crate::dto::TrustMode::First,
            )
            .unwrap();
        let sftp = services.sftp_connect(&host.id).await.unwrap();
        let saved = services
            .forward_save(ForwardProfile {
                id: String::new(),
                name: "Lock test".into(),
                host_id: host.id,
                mode: "dynamic".into(),
                bind_address: "127.0.0.1".into(),
                bind_port: 0,
                target_host: String::new(),
                target_port: 0,
            })
            .unwrap();
        let started = services
            .forward_start(&saved.profile.id, Arc::new(|_| {}))
            .await
            .unwrap();
        let mut socket = TcpStream::connect(("127.0.0.1", started.actual_port))
            .await
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("pending.bin");
        std::fs::write(&source, b"pending").unwrap();
        let job = services
            .transfer_start(
                crate::sftp_service::TransferInput {
                    connection_id: sftp.id,
                    direction: "upload".into(),
                    sources: vec![source.display().to_string()],
                    destination: sftp.home,
                    overwrite: false,
                },
                Arc::new(|_| {}),
            )
            .unwrap();
        assert!(services.network_active_count() > 0);
        services.agent.drain.cancel();
        services.close_network().await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while services.network_active_count() > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(services.sftp_list().unwrap().is_empty());
        assert_eq!(services.forward_list().unwrap()[0].status, "stopped");
        assert_eq!(
            services
                .transfer_list()
                .unwrap()
                .into_iter()
                .find(|v| v.id == job.id)
                .unwrap()
                .status,
            "cancelled"
        );
        assert!(TcpStream::connect(("127.0.0.1", started.actual_port))
            .await
            .is_err());
        let mut byte = [0];
        let result = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte))
            .await
            .unwrap();
        assert!(result.is_err() || result.unwrap() == 0);
    }
}
