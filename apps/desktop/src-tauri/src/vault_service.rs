//! The only production entry point to local data. Locked vaults have no open DB.
#[cfg(test)]
#[path = "vault_tests.rs"]
mod tests;
use crate::{
    dto::CommandError,
    service::{Services, TerminalRegistry},
};
use selai_domain::ErrorCode;
use selai_secrets::{KeyringStore, SecretError, SecretKey, SecretStore, SecretValue};
use selai_storage::Database;
use selai_vault::Vault;
use serde::Serialize;
use std::{
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

fn error(e: impl std::fmt::Display) -> CommandError {
    CommandError::new(ErrorCode::StorageIo, e.to_string())
}
fn locked() -> CommandError {
    CommandError::new(
        ErrorCode::SecretStoreLocked,
        "Unlock your vault to continue.",
    )
}
fn invalid(message: &str) -> CommandError {
    CommandError::new(ErrorCode::InvalidInput, message)
}

// Unit tests exercise the lifecycle with synthetic data independently of the
// desktop session. The Windows adapter is compiled and checked separately.
fn system_lock_required() -> bool {
    !cfg!(test) && selai_platform::must_lock()
}

struct DatabaseSecrets(Arc<Mutex<Database>>);
impl SecretStore for DatabaseSecrets {
    fn put(&self, key: &SecretKey, value: &SecretValue) -> Result<(), SecretError> {
        self.0
            .lock()
            .map_err(|_| SecretError::Locked)?
            .put_vault_secret(key.account(), value.expose_for_transport(), false)
            .map_err(|_| SecretError::Locked)
    }
    fn get(&self, key: &SecretKey) -> Result<Option<SecretValue>, SecretError> {
        self.0
            .lock()
            .map_err(|_| SecretError::Locked)?
            .vault_secret(key.account())
            .map(|v| v.map(SecretValue::new))
            .map_err(|_| SecretError::Locked)
    }
    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.0
            .lock()
            .map_err(|_| SecretError::Locked)?
            .delete_vault_secret(key.account())
            .map_err(|_| SecretError::Locked)
    }
}

struct Inner {
    vault: Option<Vault>,
    services: Option<Arc<Services>>,
    locking: bool,
    maintaining: bool,
    activity: Instant,
    timeout_minutes: u32,
    failures: u32,
    retry_at: Instant,
    cleanup_pending: bool,
    flush_required: bool,
    flush_complete: bool,
}
type StatusSink = Arc<dyn Fn(VaultStatus) + Send + Sync>;

pub struct VaultManager {
    _process_lock: std::fs::File,
    dir: PathBuf,
    inner: Mutex<Inner>,
    leases: AtomicUsize,
    status_sink: Mutex<Option<StatusSink>>,
    last_status: Mutex<Option<VaultStatus>>,
    last_published: std::sync::atomic::AtomicU64,
    terminals: TerminalRegistry,
    /// Live network sessions outlast the encrypted database lock interval.
    network: Arc<crate::network_service::NetworkRegistry>,
}

impl Drop for VaultManager {
    fn drop(&mut self) {
        self.terminals.close();
        self.network.stop_all();
    }
}
pub struct ServiceLease {
    services: Arc<Services>,
    manager: Arc<VaultManager>,
}
impl Deref for ServiceLease {
    type Target = Services;
    fn deref(&self) -> &Services {
        &self.services
    }
}
impl ServiceLease {
    /// Drop network/connect futures when locking starts, releasing any borrowed
    /// credentials. Already-running agent commands use their separate drain path.
    pub async fn run<T>(
        &self,
        future: impl std::future::Future<Output = Result<T, CommandError>>,
    ) -> Result<T, CommandError> {
        tokio::select! {biased; _=self.services.agent.drain.cancelled()=>Err(locked()),result=future=>result}
    }
}
impl Drop for ServiceLease {
    fn drop(&mut self) {
        self.manager.leases.fetch_sub(1, Ordering::SeqCst);
    }
}

struct MaintenanceLease {
    lease: ServiceLease,
}
impl Drop for MaintenanceLease {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.lease.manager.inner.lock() {
            inner.maintaining = false;
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatus {
    pub revision: u64,
    pub phase: &'static str,
    pub legacy_found: bool,
    pub timeout_minutes: u32,
    pub active_tasks: usize,
    pub terminals: usize,
    pub cleanup_pending: bool,
    pub retry_after_ms: u64,
    pub flush_required: bool,
}

impl VaultManager {
    pub fn new(dir: PathBuf) -> Result<Arc<Self>, CommandError> {
        selai_vault::private_dir(&dir).map_err(error)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("vault.lock"))
            .map_err(error)?;
        fs2::FileExt::try_lock_exclusive(&file).map_err(|_| {
            invalid(
                "Another Selai instance is using this data folder. Close it before opening Selai.",
            )
        })?;
        Ok(Arc::new(Self {
            _process_lock: file,
            dir,
            leases: AtomicUsize::new(0),
            status_sink: Mutex::new(None),
            last_status: Mutex::new(None),
            last_published: std::sync::atomic::AtomicU64::new(0),
            terminals: TerminalRegistry::default(),
            network: Arc::new(crate::network_service::NetworkRegistry::default()),
            inner: Mutex::new(Inner {
                vault: None,
                services: None,
                locking: false,
                maintaining: false,
                activity: Instant::now(),
                timeout_minutes: 15,
                failures: 0,
                retry_at: Instant::now(),
                cleanup_pending: false,
                flush_required: false,
                flush_complete: false,
            }),
        }))
    }
    pub fn status(&self) -> Result<VaultStatus, CommandError> {
        let mut last = self.last_status.lock().map_err(error)?;
        let i = self.inner.lock().map_err(error)?;
        let mut status = VaultStatus {
            revision: last.as_ref().map_or(0, |status| status.revision),
            phase: if i.locking {
                "locking"
            } else if i.services.is_some() {
                "unlocked"
            } else if self.dir.join(selai_vault::MANIFEST).exists() {
                "locked"
            } else {
                "setup"
            },
            legacy_found: self.dir.join("selai.sqlite3").exists(),
            timeout_minutes: i.timeout_minutes,
            active_tasks: i.services.as_ref().map_or(0, |s| s.agent.active_count()),
            terminals: self.terminals.count(),
            cleanup_pending: i.cleanup_pending,
            retry_after_ms: i
                .retry_at
                .saturating_duration_since(Instant::now())
                .as_millis() as u64,
            flush_required: i.flush_required,
        };
        if last.as_ref() != Some(&status) {
            status.revision = status.revision.saturating_add(1);
            *last = Some(status.clone());
        }
        Ok(status)
    }
    pub fn watch_status(&self, sink: StatusSink) -> Result<VaultStatus, CommandError> {
        *self.status_sink.lock().map_err(error)? = Some(sink);
        self.last_published.store(0, Ordering::SeqCst);
        self.status()
    }
    pub(crate) fn publish_status(&self) {
        let Ok(status) = self.status() else {
            return;
        };
        if self
            .last_published
            .fetch_max(status.revision, Ordering::SeqCst)
            >= status.revision
        {
            return;
        }
        let sink = self.status_sink.lock().ok().and_then(|sink| sink.clone());
        if let Some(sink) = sink {
            sink(status);
        }
    }
    /// Only chat snapshot persistence can enter during draining. This admits no
    /// reads, credentials, or executable work, and is serialized with DB sealing.
    pub fn snapshot_services(self: &Arc<Self>) -> Result<ServiceLease, CommandError> {
        let i = self.inner.lock().map_err(error)?;
        let services = i.services.clone().ok_or_else(locked)?;
        self.leases.fetch_add(1, Ordering::SeqCst);
        Ok(ServiceLease {
            services,
            manager: self.clone(),
        })
    }
    pub fn flush_complete(&self) -> Result<(), CommandError> {
        let mut i = self.inner.lock().map_err(error)?;
        if i.locking && i.flush_required {
            i.flush_complete = true;
        }
        Ok(())
    }
    pub fn services(self: &Arc<Self>) -> Result<ServiceLease, CommandError> {
        let i = self.inner.lock().map_err(error)?;
        if i.maintaining {
            return Err(invalid(
                "Vault maintenance is in progress. Try again shortly.",
            ));
        }
        if i.locking
            || system_lock_required()
            || i.activity.elapsed() >= Duration::from_secs(u64::from(i.timeout_minutes) * 60)
        {
            return Err(locked());
        }
        let services = i.services.clone().ok_or_else(locked)?;
        self.leases.fetch_add(1, Ordering::SeqCst);
        Ok(ServiceLease {
            services,
            manager: self.clone(),
        })
    }

    pub fn terminal_ack(&self, session_id: &str, bytes: usize) -> Result<(), CommandError> {
        self.terminals.acknowledge(session_id, bytes)
    }
    pub fn activity(&self) -> Result<(), CommandError> {
        let mut i = self.inner.lock().map_err(error)?;
        if i.locking
            || i.services.is_none()
            || i.activity.elapsed() >= Duration::from_secs(u64::from(i.timeout_minutes) * 60)
        {
            return Err(locked());
        }
        i.activity = Instant::now();
        Ok(())
    }
    pub fn idle(&self) -> bool {
        self.inner
            .lock()
            .map(|i| {
                i.services.is_some()
                    && !i.locking
                    && (system_lock_required()
                        || i.activity.elapsed()
                            >= Duration::from_secs(u64::from(i.timeout_minutes) * 60))
            })
            .unwrap_or(true)
    }
    fn install(
        self: &Arc<Self>,
        i: &mut Inner,
        vault: Vault,
        mut db: Database,
    ) -> Result<(), CommandError> {
        if !cfg!(test) && !selai_platform::acknowledge_unlock() {
            return Err(invalid("Unlock the Windows session before opening the vault. If system notifications are unavailable, restart Selai."));
        }
        let report = db
            .reconcile_after_restart(crate::service::now_ms())
            .map_err(error)?;
        let timeout = db
            .get_setting("vault.timeout_minutes")
            .map_err(error)?
            .and_then(|v| v.as_u64())
            .filter(|n| [5, 15, 30, 60].contains(n))
            .unwrap_or(15) as u32;
        let db = Arc::new(Mutex::new(db));
        let mut services =
            Services::with_shared_database(db.clone(), Arc::new(DatabaseSecrets(db)), report)?;
        services.terminal_registry = self.terminals.clone();
        services.network = self.network.clone();
        services.export_dir = Some(self.dir.join("exports"));
        services.vault_managed = true;
        let weak = Arc::downgrade(self);
        services.document_sealer = Some(Arc::new(move |bytes| {
            let manager = weak.upgrade().ok_or_else(locked)?;
            let i = manager.inner.lock().map_err(error)?;
            i.vault
                .as_ref()
                .ok_or_else(locked)?
                .seal_document(bytes)
                .map_err(error)
        }));
        let _ = services.apply_retention(crate::service::now_ms());
        i.vault = Some(vault);
        i.services = Some(Arc::new(services));
        i.locking = false;
        i.activity = Instant::now();
        i.timeout_minutes = timeout;
        i.failures = 0;
        Ok(())
    }
    pub fn initialize(
        self: &Arc<Self>,
        passphrase: Zeroizing<String>,
        legacy: &dyn SecretStore,
    ) -> Result<String, CommandError> {
        let mut i = self.inner.lock().map_err(error)?;
        if self.dir.join(selai_vault::MANIFEST).exists() {
            return Err(invalid("A vault already exists."));
        }
        let (vault, recovery) = Vault::create(&self.dir, &passphrase).map_err(error)?;
        let legacy_path = self.dir.join("selai.sqlite3");
        let legacy_digest = if legacy_path.exists() {
            let source = Database::open(&legacy_path).map_err(error)?;
            source.checkpoint().map_err(error)?;
            let before = file_digest(&legacy_path).map_err(error)?;
            source
                .export_encrypted(&vault.database_path(), vault.key())
                .map_err(error)?;
            source.checkpoint().map_err(error)?;
            drop(source);
            if file_digest(&legacy_path).map_err(error)? != before {
                return Err(invalid("The old database changed during migration. Close older Selai instances and retry."));
            }
            Some(before)
        } else {
            None
        };
        let mut db =
            Database::open_encrypted(&vault.database_path(), vault.key()).map_err(error)?;
        let mut legacy_files = serde_json::Map::new();
        if let Some(digest) = legacy_digest {
            legacy_files.insert("selai.sqlite3".into(), serde_json::Value::String(digest));
        }
        for (account, required) in db.legacy_credential_accounts().map_err(error)? {
            let key = SecretKey::from_account(&account)
                .ok_or_else(|| invalid("Invalid legacy credential address."))?;
            match legacy.get(&key).map_err(error)? {
                Some(secret) => db.put_vault_secret(&account, secret.expose_for_transport(), true).map_err(error)?,
                None if required => return Err(invalid("A saved credential is missing. Restore it in the old credential store before migrating.")),
                None => (),
            }
        }
        for (id, path) in db.external_key_files().map_err(error)? {
            // Missing keys remain external references, never silently deleted.
            if let Ok(bytes) = read_key_file(Path::new(&path)) {
                db.put_vault_secret(&format!("ssh/{id}/private_key"), &bytes, false)
                    .map_err(error)?;
                db.mark_key_managed(&id).map_err(error)?;
            }
        }
        let exports = self.dir.join("exports");
        if exports.exists() {
            for entry in std::fs::read_dir(&exports).map_err(error)? {
                let entry = entry.map_err(error)?;
                if entry.file_type().map_err(error)?.is_file()
                    && entry.path().extension().is_some_and(|e| e == "json")
                {
                    if entry.metadata().map_err(error)?.len() > 64 * 1024 * 1024 {
                        return Err(invalid("A legacy export exceeds 64 MiB. Move it to a secure location before migration."));
                    }
                    let bytes = Zeroizing::new(std::fs::read(entry.path()).map_err(error)?);
                    db.store_vault_file(&entry.file_name().to_string_lossy(), &bytes)
                        .map_err(error)?;
                    legacy_files.insert(
                        format!("exports/{}", entry.file_name().to_string_lossy()),
                        serde_json::Value::String(selai_audit::sha256_hex(&bytes)),
                    );
                }
            }
        }
        db.set_setting(
            "vault.legacy_files",
            &serde_json::Value::Object(legacy_files),
            crate::service::now_ms(),
        )
        .map_err(error)?;
        db.integrity_check().map_err(error)?;
        db.checkpoint().map_err(error)?;
        vault.commit().map_err(error)?;
        self.install(&mut i, vault, db)?;
        i.cleanup_pending = !cleanup(&self.dir, i.services.as_ref().ok_or_else(locked)?, legacy);
        Ok(recovery.to_string())
    }
    pub fn unlock(self: &Arc<Self>, passphrase: Zeroizing<String>) -> Result<(), CommandError> {
        let mut i = self.inner.lock().map_err(error)?;
        if i.services.is_some() {
            return Err(invalid("The vault is already open or locking."));
        }
        if Instant::now() < i.retry_at {
            return Err(invalid("Please wait before trying again."));
        }
        let vault = match Vault::unlock(&self.dir, &passphrase) {
            Ok(v) => v,
            Err(_) => {
                i.failures = i.failures.saturating_add(1);
                i.retry_at = Instant::now() + Duration::from_secs(1u64 << i.failures.min(5));
                return Err(invalid(
                    "Could not unlock the vault. Check your passphrase.",
                ));
            }
        };
        let db = Database::open_encrypted(&vault.database_path(), vault.key()).map_err(error)?;
        self.install(&mut i, vault, db)?;
        i.cleanup_pending = !cleanup(
            &self.dir,
            i.services.as_ref().ok_or_else(locked)?,
            &KeyringStore::new(),
        );
        Ok(())
    }
    pub fn recover(
        self: &Arc<Self>,
        recovery: Zeroizing<String>,
        passphrase: Zeroizing<String>,
    ) -> Result<String, CommandError> {
        let mut i = self.inner.lock().map_err(error)?;
        if i.services.is_some() {
            return Err(invalid("Lock the vault before recovery."));
        }
        if Instant::now() < i.retry_at {
            return Err(invalid("Please wait before trying again."));
        }
        let (vault, code) = match Vault::recover(&self.dir, &recovery, &passphrase) {
            Ok(v) => v,
            Err(e) => {
                i.failures = i.failures.saturating_add(1);
                i.retry_at = Instant::now() + Duration::from_secs(1u64 << i.failures.min(5));
                return Err(error(e));
            }
        };
        let db = Database::open_encrypted(&vault.database_path(), vault.key()).map_err(error)?;
        db.integrity_check().map_err(error)?;
        vault.commit().map_err(error)?;
        self.install(&mut i, vault, db)?;
        Ok(code.to_string())
    }
    /// Stops admission first, then drains work. It never reports locked while
    /// the encrypted connection or an operation lease still exists.
    pub async fn lock(self: Arc<Self>, force: bool) -> Result<(), CommandError> {
        let services = {
            let mut i = self.inner.lock().map_err(error)?;
            if i.locking {
                if force {
                    if let Some(s) = &i.services {
                        s.agent.cancel_all();
                    }
                }
                return Ok(());
            }
            let Some(s) = i.services.clone() else {
                return Ok(());
            };
            i.locking = true;
            i.flush_required = false;
            i.flush_complete = false;
            s.agent.drain.cancel();
            if force {
                s.agent.cancel_all();
            }
            s
        };
        self.publish_status();
        let started = Instant::now();
        let mut flush_started = None;
        loop {
            if started.elapsed() > Duration::from_secs(65) {
                services.agent.cancel_all();
            }
            {
                let mut i = self.inner.lock().map_err(error)?;
                if self.leases.load(Ordering::SeqCst) == 0 && services.agent.active_count() == 0 {
                    i.flush_required = true;
                    let at = flush_started.get_or_insert_with(Instant::now);
                    // A hung renderer cannot prevent locking. Up to three
                    // seconds are allowed to persist its final chat snapshot.
                    if i.flush_complete || at.elapsed() >= Duration::from_secs(3) {
                        services.agent.clear_sensitive();
                        services.db.lock().map_err(error)?.seal().map_err(error)?;
                        i.services = None;
                        i.vault = None;
                        i.locking = false;
                        i.flush_required = false;
                        return Ok(());
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub fn timeout(&self, minutes: u32) -> Result<(), CommandError> {
        if ![5, 15, 30, 60].contains(&minutes) {
            return Err(invalid("Choose 5, 15, 30, or 60 minutes."));
        }
        let mut i = self.inner.lock().map_err(error)?;
        if i.locking {
            return Err(locked());
        }
        i.services
            .as_ref()
            .ok_or_else(locked)?
            .db
            .lock()
            .map_err(error)?
            .set_setting(
                "vault.timeout_minutes",
                &serde_json::json!(minutes),
                crate::service::now_ms(),
            )
            .map_err(error)?;
        i.timeout_minutes = minutes;
        Ok(())
    }
    pub fn change(
        &self,
        current: Zeroizing<String>,
        new: Zeroizing<String>,
    ) -> Result<(), CommandError> {
        let mut i = self.inner.lock().map_err(error)?;
        if i.locking {
            return Err(locked());
        }
        i.vault
            .as_mut()
            .ok_or_else(locked)?
            .change_passphrase(&current, &new)
            .map_err(error)
    }
    pub fn rotate(&self, phrase: Zeroizing<String>) -> Result<String, CommandError> {
        let mut i = self.inner.lock().map_err(error)?;
        if i.locking {
            return Err(locked());
        }
        i.vault
            .as_mut()
            .ok_or_else(locked)?
            .rotate_recovery(&phrase)
            .map(|s| s.to_string())
            .map_err(error)
    }
    pub fn backup(&self, phrase: Zeroizing<String>, path: &Path) -> Result<(), CommandError> {
        self.export_destination(path)?;
        let i = self.inner.lock().map_err(error)?;
        if i.locking {
            return Err(locked());
        }
        let vault = i.vault.as_ref().ok_or_else(locked)?;
        vault.verify(&phrase).map_err(error)?;
        let db = i
            .services
            .as_ref()
            .ok_or_else(locked)?
            .db
            .lock()
            .map_err(error)?;
        let (allocated, _) = db.storage_usage().map_err(error)?;
        crate::resource_limits::check_space(&self.dir, allocated.saturating_mul(2))?;
        crate::resource_limits::check_space(
            path.parent()
                .ok_or_else(|| invalid("Choose a backup folder."))?,
            allocated,
        )?;
        vault.backup(&db, path).map_err(error)
    }

    pub fn storage_usage(self: &Arc<Self>) -> Result<String, CommandError> {
        let lease = self.services()?;
        let usage = lease
            .db
            .lock()
            .map_err(error)?
            .storage_usage()
            .map_err(error)?;
        Ok(serde_json::json!({"databaseBytes": usage.0, "reclaimableBytes": usage.1}).to_string())
    }

    pub fn compact(self: &Arc<Self>) -> Result<String, CommandError> {
        let lease = self.services()?;
        {
            let mut i = self.inner.lock().map_err(error)?;
            if i.locking
                || i.maintaining
                || self.leases.load(Ordering::SeqCst) != 1
                || lease.agent.active_count() != 0
                || lease.terminal_count() != 0
                || lease.network_active_count() != 0
            {
                return Err(invalid("Stop active tasks, terminals, transfers, and tunnels before compacting the vault."));
            }
            i.maintaining = true;
        }
        let maintenance = MaintenanceLease { lease };
        let db = maintenance.lease.db.lock().map_err(error)?;
        let (allocated, _) = db.storage_usage().map_err(error)?;
        crate::resource_limits::check_space(&self.dir, allocated.saturating_mul(2))?;
        let usage = db.compact().map_err(error)?;
        Ok(serde_json::json!({"databaseBytes":usage.0,"reclaimableBytes":usage.1}).to_string())
    }
    pub fn restore(
        self: &Arc<Self>,
        phrase: Zeroizing<String>,
        path: &Path,
    ) -> Result<(), CommandError> {
        let mut i = self.inner.lock().map_err(error)?;
        if i.services.is_some()
            || self.dir.join(selai_vault::MANIFEST).exists()
            || self.dir.join("selai.sqlite3").exists()
        {
            return Err(invalid("Restore is available only in a fresh Selai data directory. Keep the current vault and its backup safe."));
        }
        let vault = Vault::restore(&self.dir, path, &phrase).map_err(error)?;
        let db = Database::open_encrypted(&vault.database_path(), vault.key()).map_err(error)?;
        vault.commit().map_err(error)?;
        self.install(&mut i, vault, db)
    }
    pub fn retry_cleanup(&self) -> Result<bool, CommandError> {
        let mut i = self.inner.lock().map_err(error)?;
        if i.locking {
            return Err(locked());
        }
        i.cleanup_pending = !cleanup(
            &self.dir,
            i.services.as_ref().ok_or_else(locked)?,
            &KeyringStore::new(),
        );
        Ok(!i.cleanup_pending)
    }
    pub fn restore_recovery(
        self: &Arc<Self>,
        recovery: Zeroizing<String>,
        new_phrase: Zeroizing<String>,
        path: &Path,
    ) -> Result<String, CommandError> {
        let mut i = self.inner.lock().map_err(error)?;
        if i.services.is_some()
            || self.dir.join(selai_vault::MANIFEST).exists()
            || self.dir.join("selai.sqlite3").exists()
        {
            return Err(invalid("Restore requires a fresh Selai data directory."));
        }
        let (vault, code) =
            Vault::restore_with_recovery(&self.dir, path, &recovery, &new_phrase).map_err(error)?;
        let db = Database::open_encrypted(&vault.database_path(), vault.key()).map_err(error)?;
        vault.commit().map_err(error)?;
        self.install(&mut i, vault, db)?;
        Ok(code.to_string())
    }
    pub fn archives(self: &Arc<Self>) -> Result<Vec<String>, CommandError> {
        let lease = self.services()?;
        let result = lease
            .db
            .lock()
            .map_err(error)?
            .archived_vault_files()
            .map_err(error);
        result
    }
    pub fn export_archive(self: &Arc<Self>, name: &str, path: &Path) -> Result<(), CommandError> {
        self.export_destination(path)?;
        let _lease = self.services()?;
        let i = self.inner.lock().map_err(error)?;
        let data = Zeroizing::new(
            i.services
                .as_ref()
                .ok_or_else(locked)?
                .db
                .lock()
                .map_err(error)?
                .archived_vault_file(name)
                .map_err(error)?
                .ok_or_else(|| invalid("Archived export not found."))?,
        );
        let encrypted = i
            .vault
            .as_ref()
            .ok_or_else(locked)?
            .seal_document(&data)
            .map_err(error)?;
        selai_vault::write_export(path, &encrypted).map_err(error)
    }
    pub fn export_destination(&self, path: &Path) -> Result<(), CommandError> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("Choose a destination folder."))?
            .canonicalize()
            .map_err(error)?;
        let protected = self.dir.canonicalize().map_err(error)?;
        if parent.starts_with(&protected)
            || (path.exists() && path.canonicalize().map_err(error)?.starts_with(&protected))
        {
            return Err(invalid(
                "Choose a folder outside Selai's application data directory for exported files.",
            ));
        }
        Ok(())
    }
}

pub(crate) fn read_key_file(path: &Path) -> Result<Zeroizing<Vec<u8>>, CommandError> {
    let raw = path.to_string_lossy();
    let expanded = if let Some(suffix) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\"))
    {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .ok_or_else(|| invalid("Home directory could not be resolved."))?;
        PathBuf::from(home).join(suffix)
    } else {
        path.to_path_buf()
    };
    let path = expanded.as_path();
    let file = std::fs::File::open(path).map_err(error)?;
    if file.metadata().map_err(error)?.len() > 1024 * 1024 {
        return Err(invalid("Private key files must be smaller than 1 MiB."));
    }
    use std::io::Read;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > 1024 * 1024 {
        return Err(invalid("Private key is too large."));
    }
    Ok(bytes)
}

fn cleanup(dir: &Path, services: &Services, legacy: &dyn SecretStore) -> bool {
    let Ok(db) = services.db.lock() else {
        return false;
    };
    let mut okay = true;
    // Only entries with a verified encrypted copy are removed.
    match db.pending_legacy_credentials() {
        Ok(accounts) => {
            for account in accounts {
                if let Some(key) = SecretKey::from_account(&account) {
                    if legacy.delete(&key).is_ok() {
                        if db.mark_legacy_credential_removed(&account).is_err() {
                            okay = false;
                        }
                    } else {
                        okay = false;
                    }
                } else {
                    okay = false;
                }
            }
        }
        Err(_) => okay = false,
    }
    let files = match db.get_setting("vault.legacy_files") {
        Ok(Some(serde_json::Value::Object(files))) => files,
        _ => return false,
    };
    for (name, digest) in files {
        let path = dir.join(&name);
        if !path.exists() {
            continue;
        }
        let valid = name == "selai.sqlite3"
            || name
                .strip_prefix("exports/")
                .is_some_and(|n| Path::new(n).file_name().is_some_and(|file| file == n));
        // Do not discard files changed by an older instance after the snapshot.
        let wal = dir.join("selai.sqlite3-wal");
        if !valid
            || (name == "selai.sqlite3" && wal.metadata().is_ok_and(|m| m.len() > 0))
            || file_digest(&path).ok().as_deref() != digest.as_str()
        {
            okay = false;
            continue;
        }
        if std::fs::remove_file(&path).is_err() {
            okay = false;
            continue;
        }
        if name == "selai.sqlite3" {
            for suffix in ["-wal", "-shm"] {
                let sidecar = dir.join(format!("selai.sqlite3{suffix}"));
                if sidecar.exists() && std::fs::remove_file(sidecar).is_err() {
                    okay = false;
                }
            }
        }
    }
    okay
}

fn file_digest(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = Zeroizing::new(vec![0u8; 64 * 1024]);
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
