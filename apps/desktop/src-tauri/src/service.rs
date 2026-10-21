//! Application services behind the Tauri commands.
//!
//! This layer holds the rules the webview cannot override:
//! - secrets go to the active store before host metadata is committed;
//!   production uses the encrypted vault (ADR-0006);
//! - a host key is pinned only if the user confirmed the key from a recent probe;
//! - a terminal session is bound to one host and never reused for another.
//!
//! It has no Tauri types, so the end-to-end tests drive the same code the
//! commands use.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::terminal_flow::{TerminalFlow, OUTPUT_CHUNK};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use selai_domain::{
    AuthMethod, ConnectTimeout, ErrorCode, GroupName, HostGroupId, HostId, HostLabel, HostSettings,
    Hostname, Keepalive, KeyPath, Notes, Page, PageRequest, Port, SelaiError, TerminalType,
    Username,
};
use selai_secrets::{SecretKey, SecretStore, SecretValue, SshSecretKind};
use selai_ssh::{
    connect_via, probe_host_key_via, Auth, ConnectOptions, Endpoint, ObservedHostKey,
    PinnedHostKey, Session, Shell, ShellEvent, SshError,
};
use selai_storage::{Database, PinOutcome, PinnedKey, ReconcileReport, StorageError};
use tokio::sync::mpsc;

use crate::dto::{
    AppInfo, CommandError, GroupView, HostInput, HostView, KeyErrorDetails, ObservedKeyView,
    ProbeView, SshKeyView, StorageStatus, TerminalEvent, TrustMode, TrustView,
};

/// How long a probed key may be confirmed. Short on purpose.
const PROBE_TTL: Duration = Duration::from_secs(300);
const MAX_TERMINAL_DIM: u32 = 1000;

pub type EventSink = Arc<dyn Fn(TerminalEvent) + Send + Sync>;

struct CachedProbe {
    observed: ObservedHostKey,
    at: Instant,
}

pub(crate) enum TerminalInput {
    Data(Vec<u8>),
    Resize { cols: u32, rows: u32 },
}

#[derive(Clone)]
struct TerminalHandle {
    host_id: HostId,
    input: mpsc::Sender<TerminalInput>,
    stop: Arc<dyn Fn() + Send + Sync>,
    flow: Option<Arc<TerminalFlow>>,
    _slot: Arc<tokio::sync::OwnedSemaphorePermit>,
}

/// Authenticated transports outlive the vault connection. No database, vault
/// key, login password or private key is retained by this registry.
#[derive(Clone)]
pub(crate) struct TerminalRegistry {
    sessions: Arc<Mutex<HashMap<String, TerminalHandle>>>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl Default for TerminalRegistry {
    fn default() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            slots: Arc::new(tokio::sync::Semaphore::new(16)),
        }
    }
}

impl TerminalRegistry {
    pub(crate) fn count(&self) -> usize {
        self.sessions
            .lock()
            .map(|sessions| sessions.len())
            .unwrap_or(usize::MAX)
    }

    pub(crate) fn close(&self) {
        let stops: Vec<_> = self
            .sessions
            .lock()
            .map(|sessions| {
                sessions
                    .values()
                    .map(|handle| handle.stop.clone())
                    .collect()
            })
            .unwrap_or_default();
        for stop in stops {
            stop();
        }
    }

    /// Acknowledgement only releases bounded output credits. It admits no
    /// terminal input and reads no vault data, so it is also safe while locked.
    pub(crate) fn acknowledge(&self, session_id: &str, bytes: usize) -> Result<(), CommandError> {
        let sessions = self.sessions.lock().map_err(|_| poisoned())?;
        if let Some(flow) = sessions
            .get(session_id)
            .and_then(|handle| handle.flow.as_ref())
        {
            if !flow.acknowledge(bytes) {
                return Err(invalid_message("Invalid terminal output acknowledgement."));
            }
        }
        Ok(())
    }
}

pub(crate) type DocumentSealer = Arc<dyn Fn(&[u8]) -> Result<Vec<u8>, CommandError> + Send + Sync>;

pub struct Services {
    pub(crate) db: Arc<Mutex<Database>>,
    pub(crate) secrets: Arc<dyn SecretStore>,
    pub(crate) llm: Arc<selai_llm::LlmClient>,
    pub(crate) agent: Arc<crate::agent_service::AgentRegistry>,
    /// Where history exports are written. `None` for an in-memory database.
    pub(crate) export_dir: Option<std::path::PathBuf>,
    probes: Mutex<HashMap<HostId, CachedProbe>>,
    pub(crate) terminal_registry: TerminalRegistry,
    pub(crate) document_sealer: Option<DocumentSealer>,
    pub(crate) vault_managed: bool,
    pub(crate) network: Arc<crate::network_service::NetworkRegistry>,
    reconcile: ReconcileReport,
    pub(crate) ssh_auth: crate::ssh_auth::Registry,
}

impl Services {
    /// Opens the database at `path` and marks work interrupted by the previous
    /// run. Nothing is re-run.
    pub fn open(path: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self, CommandError> {
        let mut db = Database::open(path).map_err(storage_error)?;
        let reconcile = db
            .reconcile_after_restart(now_ms())
            .map_err(storage_error)?;
        let mut services = Self::with_database(db, secrets, reconcile)?;
        services.export_dir = crate::history_service::export_dir_for(path);
        // A failed retention sweep must not stop the app from opening. It runs again
        // at the next start.
        let _ = services.apply_retention(now_ms());
        Ok(services)
    }

    pub fn in_memory(secrets: Arc<dyn SecretStore>) -> Result<Self, CommandError> {
        let mut db = Database::open_in_memory().map_err(storage_error)?;
        let reconcile = db
            .reconcile_after_restart(now_ms())
            .map_err(storage_error)?;
        Self::with_database(db, secrets, reconcile)
    }

    fn with_database(
        db: Database,
        secrets: Arc<dyn SecretStore>,
        reconcile: ReconcileReport,
    ) -> Result<Self, CommandError> {
        Self::with_shared_database(Arc::new(Mutex::new(db)), secrets, reconcile)
    }

    pub(crate) fn with_shared_database(
        db: Arc<Mutex<Database>>,
        secrets: Arc<dyn SecretStore>,
        reconcile: ReconcileReport,
    ) -> Result<Self, CommandError> {
        let llm = selai_llm::LlmClient::new(selai_llm::ClientOptions::default())
            .map_err(|e| CommandError::new(e.code, e.message))?;
        Ok(Self {
            db,
            secrets,
            llm: Arc::new(llm),
            agent: Arc::new(crate::agent_service::AgentRegistry::default()),
            export_dir: None,
            probes: Mutex::new(HashMap::new()),
            terminal_registry: TerminalRegistry::default(),
            document_sealer: None,
            vault_managed: false,
            network: Arc::new(crate::network_service::NetworkRegistry::default()),
            reconcile,
            ssh_auth: crate::ssh_auth::Registry::default(),
        })
    }

    /// Replaces the provider HTTP client. Tests use this to turn off the system
    /// proxy so requests reach a local mock server.
    pub fn with_llm_client(mut self, client: selai_llm::LlmClient) -> Self {
        self.llm = Arc::new(client);
        self
    }

    pub(crate) fn terminal_count(&self) -> usize {
        self.terminal_registry.count()
    }

    pub fn app_info() -> AppInfo {
        AppInfo {
            name: "Selai",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    pub fn storage_status(&self) -> Result<StorageStatus, CommandError> {
        let db = self.lock_db()?;
        Ok(StorageStatus {
            schema_version: db.schema_version().map_err(storage_error)?,
            journal_mode: db.journal_mode().map_err(storage_error)?,
            interrupted_tasks_at_startup: self.reconcile.tasks_interrupted,
            interrupted_executions_at_startup: self.reconcile.executions_interrupted,
        })
    }

    // ---- hosts and groups -------------------------------------------------

    pub fn host_list(
        &self,
        query: Option<String>,
        include_archived: bool,
        cursor: Option<String>,
    ) -> Result<Page<HostView>, CommandError> {
        let db = self.lock_db()?;
        let page = db
            .list_hosts(
                include_archived,
                query.as_deref().filter(|q| !q.trim().is_empty()),
                &PageRequest { cursor, limit: 50 },
            )
            .map_err(storage_error)?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(|host| {
                    let mut view = HostView::from(host);
                    view.proxy_id = db.host_proxy(&view.id).map_err(storage_error)?;
                    Ok(view)
                })
                .collect::<Result<_, CommandError>>()?,
            next_cursor: page.next_cursor,
        })
    }

    pub fn ssh_key_list(&self) -> Result<Vec<SshKeyView>, CommandError> {
        self.lock_db()?
            .list_ssh_keys()
            .map(|keys| keys.into_iter().map(SshKeyView::from).collect())
            .map_err(storage_error)
    }

    pub fn ssh_key_import(
        &self,
        name: String,
        private_key_base64: String,
        passphrase: Option<String>,
    ) -> Result<SshKeyView, CommandError> {
        let name = valid_key_name(&name)?;
        let private_key_base64 = zeroize::Zeroizing::new(private_key_base64);
        let bytes = zeroize::Zeroizing::new(
            B64.decode(private_key_base64.as_bytes())
                .map_err(|_| invalid_message("Private key file is not valid base64."))?,
        );
        if bytes.len() > 1024 * 1024 {
            return Err(invalid_message("Private key must be 1 MB or smaller."));
        }
        let pem = std::str::from_utf8(&bytes)
            .map_err(|_| invalid_message("Private key must be a UTF-8 OpenSSH or PEM file."))?;
        let passphrase = passphrase.filter(|p| !p.is_empty());
        let parsed = russh::keys::decode_secret_key(pem, passphrase.as_deref()).map_err(|_| {
            invalid_message("Could not read private key. Check the file and its passphrase.")
        })?;
        selai_ssh::validate_private_key(&parsed)
            .map_err(|err| invalid_message(&err.to_string()))?;
        let view = key_view(name, parsed.public_key());
        let id: HostId = view
            .id
            .parse()
            .map_err(|_| invalid_message("Generated SSH key ID is invalid."))?;
        let private_key = SecretValue::new(bytes.to_vec());
        self.secrets
            .put(
                &SecretKey::managed_ssh_key(id, SshSecretKind::ManagedPrivateKey),
                &private_key,
            )
            .map_err(secret_error)?;
        if let Some(passphrase) = passphrase {
            if let Err(err) = self.secrets.put(
                &SecretKey::managed_ssh_key(id, SshSecretKind::ManagedKeyPassphrase),
                &SecretValue::new(passphrase),
            ) {
                let _ = self.secrets.delete(&SecretKey::managed_ssh_key(
                    id,
                    SshSecretKind::ManagedPrivateKey,
                ));
                return Err(secret_error(err));
            }
        }
        let record = selai_storage::SshKeyRecord {
            id: view.id.clone(),
            name: view.name.clone(),
            algorithm: view.algorithm.clone(),
            public_key: view.public_key.clone(),
            fingerprint_sha256: view.fingerprint_sha256.clone(),
            created_at_ms: now_ms(),
        };
        if let Err(err) = self.lock_db()?.create_ssh_key(&record) {
            let _ = self.secrets.delete(&SecretKey::managed_ssh_key(
                id,
                SshSecretKind::ManagedPrivateKey,
            ));
            let _ = self.secrets.delete(&SecretKey::managed_ssh_key(
                id,
                SshSecretKind::ManagedKeyPassphrase,
            ));
            return Err(storage_error(err));
        }
        Ok(view)
    }

    pub fn ssh_key_generate(
        &self,
        name: String,
        algorithm: String,
    ) -> Result<SshKeyView, CommandError> {
        let name = valid_key_name(&name)?;
        let rsa_bits = match algorithm.as_str() {
            "rsa-2048" => Some(2048),
            "rsa-3072" => Some(3072),
            "rsa-4096" => Some(4096),
            "rsa-8192" => Some(8192),
            _ => None,
        };
        let key = if let Some(bits) = rsa_bits {
            selai_ssh::generate_rsa_key(bits).map_err(|e| invalid_message(&e.to_string()))?
        } else {
            let algorithm = match algorithm.as_str() {
                "ed25519" => russh::keys::Algorithm::Ed25519,
                "ecdsa-p256" => russh::keys::Algorithm::Ecdsa {
                    curve: russh::keys::EcdsaCurve::NistP256,
                },
                "ecdsa-p384" => russh::keys::Algorithm::Ecdsa {
                    curve: russh::keys::EcdsaCurve::NistP384,
                },
                "ecdsa-p521" => russh::keys::Algorithm::Ecdsa {
                    curve: russh::keys::EcdsaCurve::NistP521,
                },
                _ => {
                    return Err(invalid_message(
                        "Choose Ed25519, RSA 2048/3072/4096/8192, or ECDSA P-256/P-384/P-521.",
                    ))
                }
            };
            let mut rng = rand::rng();
            russh::keys::PrivateKey::random(&mut rng, algorithm)
                .map_err(|e| invalid_message(&e.to_string()))?
        };
        let pem = key
            .to_openssh(ssh_key::LineEnding::LF)
            .map_err(|e| invalid_message(&e.to_string()))?;
        let view = key_view(name, key.public_key());
        let id: HostId = view
            .id
            .parse()
            .map_err(|_| invalid_message("Generated SSH key ID is invalid."))?;
        self.secrets
            .put(
                &SecretKey::managed_ssh_key(id, SshSecretKind::ManagedPrivateKey),
                &SecretValue::new(pem.as_bytes().to_vec()),
            )
            .map_err(secret_error)?;
        let record = selai_storage::SshKeyRecord {
            id: view.id.clone(),
            name: view.name.clone(),
            algorithm: view.algorithm.clone(),
            public_key: view.public_key.clone(),
            fingerprint_sha256: view.fingerprint_sha256.clone(),
            created_at_ms: now_ms(),
        };
        if let Err(err) = self.lock_db()?.create_ssh_key(&record) {
            let _ = self.secrets.delete(&SecretKey::managed_ssh_key(
                id,
                SshSecretKind::ManagedPrivateKey,
            ));
            return Err(storage_error(err));
        }
        Ok(view)
    }

    pub fn ssh_key_delete(&self, id: &str) -> Result<(), CommandError> {
        let id = parse_id::<HostId>(id, "SSH key")?;
        self.lock_db()?
            .delete_ssh_key(&id.to_string())
            .map_err(storage_error)?;
        self.secrets
            .delete(&SecretKey::managed_ssh_key(
                id,
                SshSecretKind::ManagedPrivateKey,
            ))
            .map_err(secret_error)?;
        self.secrets
            .delete(&SecretKey::managed_ssh_key(
                id,
                SshSecretKind::ManagedKeyPassphrase,
            ))
            .map_err(secret_error)?;
        Ok(())
    }

    pub fn host_get(&self, id: &str) -> Result<HostView, CommandError> {
        let id = parse_id::<HostId>(id, "host")?;
        let db = self.lock_db()?;
        let mut view = db
            .get_host(id)
            .map_err(storage_error)?
            .map(HostView::from)
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "host not found"))?;
        view.proxy_id = db.host_proxy(&view.id).map_err(storage_error)?;
        Ok(view)
    }

    /// Creates or updates a host. Secrets are written to the active store first;
    /// the host row is committed after, so a failed store write changes nothing.
    pub fn host_save(&self, input: HostInput) -> Result<HostView, CommandError> {
        if let Some(id) = &input.proxy_id {
            if self
                .lock_db()?
                .network_profile(id, "proxy")
                .map_err(storage_error)?
                .is_none()
            {
                return Err(invalid_message("Select an existing proxy profile."));
            }
        }
        let mut settings = input.to_settings()?;
        let now = now_ms();
        let previous = match input.id.as_deref() {
            Some(raw) => {
                let id = parse_id::<HostId>(raw, "host")?;
                let record = self
                    .lock_db()?
                    .get_host(id)
                    .map_err(storage_error)?
                    .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "host not found"))?;
                if record.is_local {
                    return Err(CommandError::new(
                        ErrorCode::InvalidInput,
                        "this computer cannot be edited",
                    ));
                }
                Some(record)
            }
            None => None,
        };
        let id = previous.as_ref().map_or_else(HostId::new, |p| p.id);
        let auth_changed = previous.as_ref().is_some_and(|p| p.auth != settings.auth);

        // 1. Stage secrets in the active store (the vault in production).
        let mut configured = previous.as_ref().is_some_and(|p| p.credential_configured);
        let password_key = SecretKey::ssh(id, SshSecretKind::Password);
        let passphrase_key = SecretKey::ssh(id, SshSecretKind::KeyPassphrase);
        let mut written: Vec<SecretKey> = Vec::new();
        if self.vault_managed && settings.auth == AuthMethod::PrivateKey {
            let path = settings
                .private_key_path
                .as_ref()
                .ok_or_else(|| {
                    CommandError::new(ErrorCode::InvalidInput, "Choose a private key file.")
                })?
                .as_str();
            let key = SecretKey::ssh(id, SshSecretKind::PrivateKey);
            if let Some(key_id) = path.strip_prefix("vault://key:") {
                let managed = parse_id::<HostId>(key_id, "SSH key")?;
                if self
                    .lock_db()?
                    .ssh_key(&managed.to_string())
                    .map_err(storage_error)?
                    .is_none()
                    || self
                        .secrets
                        .get(&SecretKey::managed_ssh_key(
                            managed,
                            SshSecretKind::ManagedPrivateKey,
                        ))
                        .map_err(secret_error)?
                        .is_none()
                {
                    return Err(invalid_message(
                        "Selected SSH key is unavailable in the vault.",
                    ));
                }
                settings.private_key_path = Some(KeyPath::parse(path).map_err(invalid)?);
                configured = true;
            } else if path.starts_with("vault://") {
                if path != format!("vault://{id}")
                    || self.secrets.get(&key).map_err(secret_error)?.is_none()
                {
                    return Err(CommandError::new(
                        ErrorCode::InvalidInput,
                        "Managed private key was not found.",
                    ));
                }
            } else {
                let bytes = crate::vault_service::read_key_file(Path::new(path))?;
                self.secrets
                    .put(&key, &SecretValue::new(bytes.to_vec()))
                    .map_err(secret_error)?;
                written.push(key);
                settings.private_key_path =
                    Some(KeyPath::parse(&format!("vault://{id}")).map_err(invalid)?);
            }
        }
        match settings.auth {
            AuthMethod::Password => {
                if let Some(pw) = non_empty(input.password.as_deref()) {
                    self.secrets
                        .put(&password_key, &SecretValue::new(pw))
                        .map_err(secret_error)?;
                    written.push(password_key.clone());
                    configured = true;
                }
                if auth_changed {
                    self.secrets.delete(&passphrase_key).map_err(secret_error)?;
                    configured = written.contains(&password_key);
                }
            }
            AuthMethod::PrivateKey => {
                if let Some(pp) = non_empty(input.key_passphrase.as_deref()) {
                    self.secrets
                        .put(&passphrase_key, &SecretValue::new(pp))
                        .map_err(secret_error)?;
                    written.push(passphrase_key.clone());
                    configured = true;
                }
                if auth_changed {
                    self.secrets.delete(&password_key).map_err(secret_error)?;
                    configured = settings
                        .private_key_path
                        .as_ref()
                        .is_some_and(|p| p.as_str().starts_with("vault://"))
                        || written.contains(&passphrase_key);
                }
            }
            _ => {
                if auth_changed {
                    self.secrets.delete(&password_key).map_err(secret_error)?;
                    self.secrets.delete(&passphrase_key).map_err(secret_error)?;
                }
                configured = false;
            }
        }

        // 2. Commit the host row.
        let saved = {
            let mut db = self.lock_db()?;
            let result = match previous {
                Some(ref p) => db.update_host(
                    id,
                    &settings,
                    input.expected_updated_at_ms.unwrap_or(p.updated_at_ms),
                    now,
                ),
                None => db.create_host(id, &settings, now),
            };
            match result {
                Ok(record) => record,
                Err(err) => {
                    // A new host has no earlier state to keep, so its secrets are removed.
                    // For an update the store keeps the newer value; the flag stays as it was.
                    if previous.is_none() {
                        for key in &written {
                            let _ = self.secrets.delete(key);
                        }
                    }
                    return Err(storage_error(err));
                }
            }
        };

        // 3. Record whether a credential is configured.
        self.lock_db()?
            .set_credential_configured(id, configured, now)
            .map_err(storage_error)?;
        self.lock_db()?
            .set_host_proxy(&id.to_string(), input.proxy_id.as_deref())
            .map_err(storage_error)?;
        self.host_get(&saved.id.to_string())
    }

    /// Archives a host, deletes its stored secrets, keeps history and pins.
    pub fn host_archive(&self, id: &str) -> Result<(), CommandError> {
        let id = parse_id::<HostId>(id, "host")?;
        self.lock_db()?
            .archive_host(id, now_ms())
            .map_err(storage_error)?;
        self.lock_db()?
            .set_host_proxy(&id.to_string(), None)
            .map_err(storage_error)?;
        self.secrets
            .delete(&SecretKey::ssh(id, SshSecretKind::Password))
            .map_err(secret_error)?;
        self.secrets
            .delete(&SecretKey::ssh(id, SshSecretKind::KeyPassphrase))
            .map_err(secret_error)?;
        self.secrets
            .delete(&SecretKey::ssh(id, SshSecretKind::SudoPassword))
            .map_err(secret_error)?;
        self.secrets
            .delete(&SecretKey::ssh(id, SshSecretKind::PrivateKey))
            .map_err(secret_error)?;
        self.lock_db()?
            .set_credential_configured(id, false, now_ms())
            .map_err(storage_error)?;
        Ok(())
    }

    pub fn host_move_group(
        &self,
        id: &str,
        group_id: Option<&str>,
        expected_updated_at_ms: i64,
    ) -> Result<HostView, CommandError> {
        let id = parse_id::<HostId>(id, "host")?;
        let group = group_id
            .map(|value| parse_id::<HostGroupId>(value, "group"))
            .transpose()?;
        self.lock_db()?
            .move_host_to_group(id, group, expected_updated_at_ms, now_ms())
            .map_err(storage_error)?;
        self.host_get(&id.to_string())
    }

    pub fn group_list(&self) -> Result<Vec<GroupView>, CommandError> {
        let db = self.lock_db()?;
        Ok(db
            .list_groups()
            .map_err(storage_error)?
            .into_iter()
            .map(GroupView::from)
            .collect())
    }

    /// Explicit reveal in the unlocked profile editor. Ordinary host responses
    /// never contain credentials, and this value is not written to audit/chat.
    pub fn host_reveal_password(&self, host_id: &str) -> Result<Option<String>, CommandError> {
        let id = parse_id::<HostId>(host_id, "host")?;
        self.refuse_local(id)?;
        let (host, _) = self.active_host_and_pins(id)?;
        if host.auth != AuthMethod::Password {
            return Err(invalid_message(
                "This profile does not use password authentication.",
            ));
        }
        self.secrets
            .get(&SecretKey::ssh(id, SshSecretKind::Password))
            .map_err(secret_error)?
            .map(|secret| {
                std::str::from_utf8(secret.expose_for_transport())
                    .map(str::to_owned)
                    .map_err(|_| invalid_message("Saved password is not valid UTF-8."))
            })
            .transpose()
    }

    pub fn group_create(&self, name: &str, sort_order: i64) -> Result<GroupView, CommandError> {
        let name = GroupName::parse(name).map_err(invalid)?;
        let db = self.lock_db()?;
        let group = db
            .create_group(HostGroupId::new(), &name, sort_order, now_ms())
            .map_err(storage_error)?;
        Ok(GroupView::from(group))
    }

    pub fn group_rename(&self, id: &str, name: &str) -> Result<GroupView, CommandError> {
        let id = parse_id::<HostGroupId>(id, "group")?;
        let name = GroupName::parse(name).map_err(invalid)?;
        self.lock_db()?
            .rename_group(id, &name)
            .map(GroupView::from)
            .map_err(storage_error)
    }

    pub fn group_delete(&self, id: &str) -> Result<(), CommandError> {
        let id = parse_id::<HostGroupId>(id, "group")?;
        self.lock_db()?
            .delete_group(id, now_ms())
            .map_err(storage_error)
    }

    // ---- host-key trust ----------------------------------------------------

    /// Connects far enough to read the server key, compares it with the pins,
    /// and remembers it for a short time so `host_trust` can confirm it.
    pub async fn host_probe(&self, host_id: &str) -> Result<ProbeView, CommandError> {
        let id = parse_id::<HostId>(host_id, "host")?;
        self.refuse_local(id)?;
        let (host, pins) = self.active_host_and_pins(id)?;
        let endpoint = Endpoint::new(host.hostname.clone(), host.port);
        let observed = probe_host_key_via(
            &endpoint,
            Duration::from_secs(u64::from(host.connect_timeout_seconds)),
            self.proxy_for_host(id)?,
        )
        .await
        .map_err(|e| ssh_error(&e))?;

        let (status, pinned_fp) = compare_with_pins(&observed, &pins);
        self.probes.lock().map_err(|_| poisoned())?.insert(
            id,
            CachedProbe {
                observed: observed.clone(),
                at: Instant::now(),
            },
        );
        Ok(ProbeView {
            observed: observed_view(&observed),
            status,
            pinned_fingerprint_sha256: pinned_fp,
        })
    }

    /// When a connection stops at an unknown or changed host key, keeps the key the
    /// server presented as a fresh probe. The user can then review and trust it in
    /// the connect flow, without a separate probe. Trusting still needs the exact
    /// fingerprint that was shown, within the probe lifetime.
    pub(crate) fn remember_observed_key(&self, id: HostId, err: &SshError) {
        let observed = match err {
            SshError::HostKeyUnknown(observed) => observed.clone(),
            SshError::HostKeyChanged { observed, .. } => observed.clone(),
            _ => return,
        };
        if let Ok(mut probes) = self.probes.lock() {
            probes.insert(
                id,
                CachedProbe {
                    observed,
                    at: Instant::now(),
                },
            );
        }
    }

    /// Pins a key that the user confirmed from a fresh probe. The fingerprint
    /// must match the probe; the webview cannot supply a key of its own.
    pub fn host_trust(
        &self,
        host_id: &str,
        fingerprint_sha256: &str,
        mode: TrustMode,
    ) -> Result<TrustView, CommandError> {
        let id = parse_id::<HostId>(host_id, "host")?;
        let observed = {
            let mut probes = self.probes.lock().map_err(|_| poisoned())?;
            let cached = probes.get(&id).ok_or_else(|| {
                CommandError::new(
                    ErrorCode::InvalidInput,
                    "probe the host key before trusting it",
                )
            })?;
            if cached.at.elapsed() > PROBE_TTL {
                probes.remove(&id);
                return Err(CommandError::new(
                    ErrorCode::InvalidInput,
                    "the probed key expired; probe again",
                ));
            }
            if cached.observed.fingerprint_sha256 != fingerprint_sha256 {
                return Err(CommandError::new(
                    ErrorCode::InvalidInput,
                    "the fingerprint does not match the key from the last probe",
                ));
            }
            cached.observed.clone()
        };
        let key = PinnedKey {
            algorithm: observed.algorithm.clone(),
            public_key_base64: observed.public_key_base64.clone(),
            fingerprint_sha256: observed.fingerprint_sha256.clone(),
            first_seen_at_ms: now_ms(),
        };
        let outcome = {
            let mut db = self.lock_db()?;
            match mode {
                TrustMode::First => db.pin_host_key(id, &key).map_err(storage_error)?,
                TrustMode::Replace => db
                    .rotate_host_key(id, &key, now_ms())
                    .map_err(storage_error)?,
            }
        };
        self.probes.lock().map_err(|_| poisoned())?.remove(&id);
        let outcome = match outcome {
            PinOutcome::Pinned => "pinned",
            PinOutcome::Rotated { .. } => "rotated",
            PinOutcome::Unchanged => "unchanged",
        };
        Ok(TrustView {
            outcome,
            fingerprint_sha256: observed.fingerprint_sha256,
        })
    }

    // ---- terminal sessions -------------------------------------------------

    /// Opens an authenticated shell. The returned id is the only handle the
    /// webview gets; it is bound to `host_id` for its whole life.
    pub async fn terminal_connect(
        &self,
        host_id: &str,
        cols: u32,
        rows: u32,
        sink: EventSink,
    ) -> Result<String, CommandError> {
        // Headless consumers drain synchronously; desktop IPC uses byte credits.
        self.connect_terminal(host_id, cols, rows, sink, None).await
    }

    pub async fn terminal_connect_with_flow(
        &self,
        host_id: &str,
        cols: u32,
        rows: u32,
        sink: EventSink,
        flow: Arc<TerminalFlow>,
    ) -> Result<String, CommandError> {
        self.connect_terminal(host_id, cols, rows, sink, Some(flow))
            .await
    }

    async fn connect_terminal(
        &self,
        host_id: &str,
        cols: u32,
        rows: u32,
        sink: EventSink,
        flow: Option<Arc<TerminalFlow>>,
    ) -> Result<String, CommandError> {
        let slot = Arc::new(
            self.terminal_registry
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| {
                    CommandError::new(
                        ErrorCode::InvalidInput,
                        "Close a terminal before opening another (limit 16).",
                    )
                })?,
        );
        let id = parse_id::<HostId>(host_id, "host")?;
        let cols = bounded_dim(cols)?;
        let rows = bounded_dim(rows)?;
        let (host, pins) = self.active_host_and_pins(id)?;
        if host.is_local {
            return self.open_local_terminal(id, cols, rows, sink, flow, slot);
        }
        let auth = self.load_auth(id, &host)?;

        let options = ConnectOptions {
            endpoint: Endpoint::new(host.hostname.clone(), host.port),
            username: host.username.clone(),
            auth,
            pinned: pins,
            connect_timeout: Duration::from_secs(u64::from(host.connect_timeout_seconds)),
            keepalive: match host.keepalive_seconds {
                0 => None,
                s => Some(Duration::from_secs(u64::from(s))),
            },
        };
        let session = match connect_via(options, self.proxy_for_host(id)?).await {
            Ok(session) => session,
            Err(e) => {
                self.remember_observed_key(id, &e);
                return Err(ssh_error(&e));
            }
        };
        let shell = session
            .open_shell(cols, rows, &host.terminal_type)
            .await
            .map_err(|e| ssh_error(&e))?;

        let session_id = HostId::new().to_string();
        let (tx, rx) = mpsc::channel(64);
        let shutdown = tokio_util::sync::CancellationToken::new();
        let stop = shutdown.clone();
        let stop_flow = flow.clone();
        self.terminal_registry
            .sessions
            .lock()
            .map_err(|_| poisoned())?
            .insert(
                session_id.clone(),
                TerminalHandle {
                    host_id: id,
                    input: tx,
                    stop: Arc::new(move || {
                        if let Some(flow) = &stop_flow {
                            flow.close();
                        }
                        stop.cancel();
                    }),
                    flow: flow.clone(),
                    _slot: slot,
                },
            );
        let _ = self.lock_db()?.mark_connected(id, now_ms());

        tokio::spawn(run_terminal(
            session_id.clone(),
            session,
            shell,
            rx,
            sink,
            Arc::clone(&self.terminal_registry.sessions),
            shutdown,
            flow,
        ));
        Ok(session_id)
    }

    /// A shell on this computer, in the same session map as SSH terminals.
    fn open_local_terminal(
        &self,
        id: HostId,
        cols: u32,
        rows: u32,
        sink: EventSink,
        flow: Option<Arc<TerminalFlow>>,
        slot: Arc<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<String, CommandError> {
        let session_id = HostId::new().to_string();
        let (tx, rx) = mpsc::channel(64);
        let mut map = self
            .terminal_registry
            .sessions
            .lock()
            .map_err(|_| poisoned())?;
        let terminals = Arc::clone(&self.terminal_registry.sessions);
        let ended = session_id.clone();
        let stop = crate::local_terminal::spawn_local_shell(
            cols,
            rows,
            rx,
            sink,
            flow.clone(),
            Box::new(move || {
                if let Ok(mut map) = terminals.lock() {
                    map.remove(&ended);
                }
            }),
        )?;
        map.insert(
            session_id.clone(),
            TerminalHandle {
                host_id: id,
                input: tx,
                stop,
                flow,
                _slot: slot,
            },
        );
        drop(map);
        let _ = self.lock_db()?.mark_connected(id, now_ms());
        Ok(session_id)
    }

    /// The built-in "This computer" entry.
    pub fn local_host(&self) -> Result<HostView, CommandError> {
        Ok(HostView::from(
            self.lock_db()?.local_host().map_err(storage_error)?,
        ))
    }

    pub async fn terminal_input(&self, session_id: &str, data: String) -> Result<(), CommandError> {
        let handle = self.terminal(session_id)?;
        handle
            .input
            .send(TerminalInput::Data(data.into_bytes()))
            .await
            .map_err(|_| {
                CommandError::new(ErrorCode::SshChannelClosed, "terminal session has ended")
            })
    }

    pub fn terminal_ack(&self, session_id: &str, bytes: usize) -> Result<(), CommandError> {
        self.terminal_registry.acknowledge(session_id, bytes)
    }

    pub async fn terminal_resize(
        &self,
        session_id: &str,
        cols: u32,
        rows: u32,
    ) -> Result<(), CommandError> {
        let cols = bounded_dim(cols)?;
        let rows = bounded_dim(rows)?;
        let handle = self.terminal(session_id)?;
        handle
            .input
            .send(TerminalInput::Resize { cols, rows })
            .await
            .map_err(|_| {
                CommandError::new(ErrorCode::SshChannelClosed, "terminal session has ended")
            })
    }

    pub async fn terminal_disconnect(&self, session_id: &str) -> Result<(), CommandError> {
        let handle = self.terminal(session_id)?;
        (handle.stop)();
        Ok(())
    }

    /// The host a live terminal session is bound to. Used by tests and by the
    /// UI to show which host a tab belongs to.
    pub fn terminal_host(&self, session_id: &str) -> Option<HostId> {
        self.terminal_registry
            .sessions
            .lock()
            .ok()?
            .get(session_id)
            .map(|h| h.host_id)
    }

    // ---- helpers -----------------------------------------------------------

    /// Host-key and credential operations do not apply to this computer.
    fn refuse_local(&self, id: HostId) -> Result<(), CommandError> {
        let local = self
            .lock_db()?
            .get_host(id)
            .map_err(storage_error)?
            .is_some_and(|h| h.is_local);
        if local {
            return Err(CommandError::new(
                ErrorCode::InvalidInput,
                "this computer has no SSH host key or saved credentials",
            ));
        }
        Ok(())
    }

    fn lock_db(&self) -> Result<std::sync::MutexGuard<'_, Database>, CommandError> {
        self.db.lock().map_err(|_| poisoned())
    }

    pub(crate) fn lock_db_pub(&self) -> Result<std::sync::MutexGuard<'_, Database>, CommandError> {
        self.lock_db()
    }

    fn terminal(&self, session_id: &str) -> Result<TerminalHandle, CommandError> {
        self.terminal_registry
            .sessions
            .lock()
            .map_err(|_| poisoned())?
            .get(session_id)
            .cloned()
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "no such terminal session"))
    }

    pub(crate) fn active_host_and_pins(
        &self,
        id: HostId,
    ) -> Result<(selai_storage::HostRecord, Vec<PinnedHostKey>), CommandError> {
        let db = self.lock_db()?;
        let host = db
            .get_host(id)
            .map_err(storage_error)?
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "host not found"))?;
        if host.archived_at_ms.is_some() {
            return Err(CommandError::new(
                ErrorCode::TaskScopeInvalid,
                "host is archived",
            ));
        }
        let pins = db
            .active_pinned_keys(id)
            .map_err(storage_error)?
            .into_iter()
            .map(|k| PinnedHostKey {
                algorithm: k.algorithm,
                public_key_base64: k.public_key_base64,
                fingerprint_sha256: k.fingerprint_sha256,
            })
            .collect();
        Ok((host, pins))
    }

    pub(crate) fn load_auth(
        &self,
        id: HostId,
        host: &selai_storage::HostRecord,
    ) -> Result<Auth, CommandError> {
        if host.auth.uses_native() {
            return self
                .ssh_auth
                .register(
                    id.to_string(),
                    host.label.clone(),
                    host.auth,
                    host.private_key_path.clone(),
                    self.agent.drain.clone(),
                )
                .map(Auth::Native);
        }
        match host.auth {
            AuthMethod::Password => {
                let secret = self
                    .secrets
                    .get(&SecretKey::ssh(id, SshSecretKind::Password))
                    .map_err(secret_error)?
                    .ok_or_else(|| {
                        CommandError::new(
                            ErrorCode::InvalidInput,
                            "no password saved for this host; edit the host to add one",
                        )
                    })?;
                Ok(Auth::Password(secret))
            }
            AuthMethod::PrivateKey => {
                let path = host.private_key_path.clone().ok_or_else(|| {
                    CommandError::new(ErrorCode::InvalidInput, "host has no private key path")
                })?;
                let passphrase = self
                    .secrets
                    .get(&SecretKey::ssh(id, SshSecretKind::KeyPassphrase))
                    .map_err(secret_error)?;
                if path.starts_with("vault://") {
                    if let Some(key_id) = path.strip_prefix("vault://key:") {
                        let key_id = parse_id::<HostId>(key_id, "SSH key")?;
                        let pem = self
                            .secrets
                            .get(&SecretKey::managed_ssh_key(
                                key_id,
                                SshSecretKind::ManagedPrivateKey,
                            ))
                            .map_err(secret_error)?
                            .ok_or_else(|| invalid_message("Managed SSH key is missing."))?;
                        let passphrase = self
                            .secrets
                            .get(&SecretKey::managed_ssh_key(
                                key_id,
                                SshSecretKind::ManagedKeyPassphrase,
                            ))
                            .map_err(secret_error)?;
                        return Ok(Auth::PrivateKeyData { pem, passphrase });
                    }
                    let pem = self
                        .secrets
                        .get(&SecretKey::ssh(id, SshSecretKind::PrivateKey))
                        .map_err(secret_error)?
                        .ok_or_else(|| {
                            CommandError::new(
                                ErrorCode::InvalidInput,
                                "Managed private key is missing.",
                            )
                        })?;
                    return Ok(Auth::PrivateKeyData { pem, passphrase });
                }
                Ok(Auth::PrivateKey {
                    path: path.into(),
                    passphrase,
                })
            }
            _ => Err(invalid_message("Unsupported authentication method")),
        }
    }
}

/// One terminal session: pumps input to the PTY and PTY output to the sink.
/// It owns the session, so the session always closes when the loop ends.
#[allow(clippy::too_many_arguments)]
async fn run_terminal(
    session_id: String,
    session: Session,
    mut shell: Shell,
    mut input: mpsc::Receiver<TerminalInput>,
    sink: EventSink,
    terminals: Arc<Mutex<HashMap<String, TerminalHandle>>>,
    shutdown: tokio_util::sync::CancellationToken,
    flow: Option<Arc<TerminalFlow>>,
) {
    let mut exit_status: Option<u32> = None;
    let pump = async {
        loop {
            tokio::select! {
                command = input.recv() => match command {
                    Some(TerminalInput::Data(bytes)) => {
                        if shell.write(&bytes).await.is_err() {
                            break;
                        }
                    }
                    Some(TerminalInput::Resize { cols, rows }) => {
                        let _ = shell.resize(cols, rows).await;
                    }
                    None => {
                        let _ = shell.close().await;
                        break;
                    }
                },
                event = shell.next_event() => match event {
                    ShellEvent::Data(bytes) => {
                        for chunk in bytes.chunks(OUTPUT_CHUNK) {
                            if let Some(flow) = &flow {
                                if !flow.reserve(chunk.len()).await { return; }
                            }
                            sink(TerminalEvent::Data { data: B64.encode(chunk) });
                        }
                    }
                    ShellEvent::Exit { status } => {
                        exit_status = Some(status);
                        sink(TerminalEvent::Exit { status });
                        break;
                    }
                    ShellEvent::Refused(message) => {
                        sink(TerminalEvent::Error {
                            code: ErrorCode::SshChannelClosed,
                            message,
                        });
                    }
                    ShellEvent::Closed => break,
                },
            }
        }
    };
    tokio::select! {biased; _=shutdown.cancelled()=>{},_=pump=>{}}
    if let Some(flow) = &flow {
        flow.close();
    }
    // The channel is already gone; a failed disconnect has no further effect.
    let _ = tokio::time::timeout(Duration::from_secs(1), session.disconnect()).await;
    if let Ok(mut map) = terminals.lock() {
        map.remove(&session_id);
    }
    sink(TerminalEvent::Closed { exit_status });
}

fn compare_with_pins(
    observed: &ObservedHostKey,
    pins: &[PinnedHostKey],
) -> (&'static str, Option<String>) {
    if let Some(pin) = pins.iter().find(|p| p.algorithm == observed.algorithm) {
        return if pin.public_key_base64 == observed.public_key_base64 {
            ("trusted", None)
        } else {
            ("changed", Some(pin.fingerprint_sha256.clone()))
        };
    }
    match pins.first() {
        Some(other) => ("changed", Some(other.fingerprint_sha256.clone())),
        None => ("unknown", None),
    }
}

fn observed_view(k: &ObservedHostKey) -> ObservedKeyView {
    ObservedKeyView {
        algorithm: k.algorithm.clone(),
        fingerprint_sha256: k.fingerprint_sha256.clone(),
        public_key_base64: k.public_key_base64.clone(),
    }
}

fn valid_key_name(name: &str) -> Result<String, CommandError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
        return Err(invalid_message(
            "Key name must contain 1 to 80 printable characters.",
        ));
    }
    Ok(name.to_owned())
}

fn key_view(name: String, public: &russh::keys::PublicKey) -> SshKeyView {
    let public_key = public.to_openssh().unwrap_or_default();
    let algorithm = public_key
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_owned();
    SshKeyView {
        id: HostId::new().to_string(),
        name,
        algorithm,
        public_key,
        fingerprint_sha256: public.fingerprint(russh::keys::HashAlg::Sha256).to_string(),
        created_at_ms: now_ms(),
    }
}

impl HostInput {
    fn to_settings(&self) -> Result<HostSettings, CommandError> {
        let auth = AuthMethod::parse(&self.auth_method).ok_or_else(|| {
            CommandError::new(ErrorCode::InvalidInput, "Unknown SSH authentication method")
        })?;
        let private_key_path = match self.private_key_path.as_deref() {
            Some(p) if !p.is_empty() => Some(KeyPath::parse(p).map_err(invalid)?),
            _ => None,
        };
        let settings = HostSettings {
            label: HostLabel::parse(&self.label).map_err(invalid)?,
            hostname: Hostname::parse(&self.hostname).map_err(invalid)?,
            port: Port::new(self.port).map_err(invalid)?,
            username: Username::parse(&self.username).map_err(invalid)?,
            auth,
            private_key_path,
            group_id: match self.group_id.as_deref() {
                Some(g) if !g.is_empty() => Some(parse_id::<HostGroupId>(g, "group")?),
                _ => None,
            },
            connect_timeout: ConnectTimeout::new(self.connect_timeout_seconds).map_err(invalid)?,
            keepalive: Keepalive::new(self.keep_alive_seconds).map_err(invalid)?,
            terminal_type: TerminalType::parse(&self.terminal_type).map_err(invalid)?,
            notes: match self.notes.as_deref() {
                Some(n) if !n.is_empty() => Some(Notes::parse(n).map_err(invalid)?),
                _ => None,
            },
        };
        settings.check_auth_shape().map_err(invalid)?;
        if auth.uses_native()
            && (non_empty(self.password.as_deref()).is_some()
                || non_empty(self.key_passphrase.as_deref()).is_some())
        {
            return Err(invalid_message("Native SSH credentials are requested at connection time, not stored in this profile."));
        }
        if auth == AuthMethod::PrivateKey && self.password.as_deref().is_some_and(|p| !p.is_empty())
        {
            return Err(CommandError::new(
                ErrorCode::InvalidInput,
                "a password cannot be saved for key authentication",
            ));
        }
        if auth == AuthMethod::Password
            && self
                .key_passphrase
                .as_deref()
                .is_some_and(|p| !p.is_empty())
        {
            return Err(CommandError::new(
                ErrorCode::InvalidInput,
                "a key passphrase cannot be saved for password authentication",
            ));
        }
        Ok(settings)
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.is_empty())
}

fn bounded_dim(value: u32) -> Result<u32, CommandError> {
    if (1..=MAX_TERMINAL_DIM).contains(&value) {
        Ok(value)
    } else {
        Err(CommandError::new(
            ErrorCode::InvalidInput,
            "terminal size is out of range",
        ))
    }
}

fn parse_id<T: std::str::FromStr>(raw: &str, what: &str) -> Result<T, CommandError> {
    raw.parse::<T>()
        .map_err(|_| CommandError::new(ErrorCode::InvalidInput, format!("invalid {what} id")))
}

fn invalid(err: impl SelaiError + std::fmt::Display) -> CommandError {
    CommandError::new(err.code(), err.to_string())
}

fn invalid_message(message: &str) -> CommandError {
    CommandError::new(ErrorCode::InvalidInput, message)
}

fn storage_error(err: StorageError) -> CommandError {
    let code = err.code();
    let message = match err {
        StorageError::Conflict(msg) => msg,
        StorageError::NotFound(_) => "not found".to_owned(),
        other => other.to_string(),
    };
    CommandError::new(code, message)
}

fn secret_error(err: selai_secrets::SecretError) -> CommandError {
    CommandError::new(err.code(), err.to_string())
}

fn poisoned() -> CommandError {
    CommandError::new(ErrorCode::Internal, "internal state is unavailable")
}

pub(crate) fn ssh_error(err: &SshError) -> CommandError {
    let mut out = CommandError::new(err.code(), err.to_string());
    match err {
        SshError::HostKeyUnknown(observed) => {
            out.details = Some(KeyErrorDetails {
                algorithm: observed.algorithm.clone(),
                fingerprint_sha256: observed.fingerprint_sha256.clone(),
                pinned_fingerprint_sha256: None,
            });
        }
        SshError::HostKeyChanged {
            observed,
            pinned_fingerprint,
        } => {
            out.details = Some(KeyErrorDetails {
                algorithm: observed.algorithm.clone(),
                fingerprint_sha256: observed.fingerprint_sha256.clone(),
                pinned_fingerprint_sha256: Some(pinned_fingerprint.clone()),
            });
        }
        _ => {}
    }
    out
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}
