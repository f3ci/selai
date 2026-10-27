//! Native file manager and bounded streaming transfers. File bytes never cross IPC.
use crate::{
    dto::CommandError,
    network_service::{failed, invalid, state_lock, NetworkRegistry},
    resource_limits::{check_space, MetadataBudget},
    service::Services,
};
use russh_sftp::{
    client::{error::Error as SftpError, RawSftpSession, SftpSession},
    protocol::{OpenFlags, StatusCode},
};
use selai_domain::HostId;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, UNIX_EPOCH},
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};
use tokio_util::sync::CancellationToken;

const DIRECTORY_LIMIT: usize = 20_000;
const PLAN_LIMIT: usize = 100_000;
pub(crate) struct SftpConnection {
    pub session: Arc<selai_ssh::Session>,
    client: Arc<SftpSession>,
    pub stop: CancellationToken,
    host_id: String,
    home: String,
}
pub(crate) struct TransferJob {
    pub stop: CancellationToken,
    pub view: Mutex<TransferView>,
    event: Mutex<TransferSink>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub size: u64,
    pub modified_ms: Option<u64>,
    pub permissions: Option<u32>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryView {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<FileEntry>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpView {
    pub id: String,
    pub host_id: String,
    pub home: String,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferView {
    pub id: String,
    pub connection_id: String,
    pub direction: String,
    pub status: String,
    pub current_file: String,
    pub destination: String,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub total_files: u64,
    pub completed_files: u64,
    pub skipped_files: u64,
    pub bytes_per_second: u64,
    pub error: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferInput {
    pub connection_id: String,
    pub direction: String,
    pub sources: Vec<String>,
    pub destination: String,
    #[serde(default)]
    pub overwrite: bool,
}
pub type TransferSink = Arc<dyn Fn(TransferView) + Send + Sync>;

impl TransferJob {
    fn spawn(
        self: Arc<Self>,
        registry: Arc<NetworkRegistry>,
        sink: TransferSink,
        transfer: impl std::future::Future<Output = Result<(), CommandError>> + Send + 'static,
    ) {
        // transfer_start is a synchronous IPC command, so its caller may be the
        // WebView thread with no entered Tokio runtime. Use Tauri's runtime
        // explicitly instead of tokio::spawn, which would panic on that thread.
        tauri::async_runtime::spawn(async move {
            let permit = tokio::select! {biased;_=self.stop.cancelled()=>None,p=registry.transfer_slots.acquire()=>p.ok()};
            let result = if permit.is_some() {
                transfer.await
            } else {
                Err(invalid("Transfer cancelled."))
            };
            drop(permit);
            let finished = if let Ok(mut view) = self.view.lock() {
                view.status = if self.stop.is_cancelled() {
                    "cancelled"
                } else if result.is_ok() {
                    "completed"
                } else {
                    "failed"
                }
                .into();
                view.error = if self.stop.is_cancelled() {
                    None
                } else {
                    result.err().map(|e| e.message)
                };
                Some(view.clone())
            } else {
                None
            };
            if let Some(view) = finished {
                sink(view);
            }
        });
    }
}
fn safe_name(name: &str) -> Result<(), CommandError> {
    if name.is_empty() || [".", ".."].contains(&name) || name.contains(['/', '\\', '\0']) {
        return Err(invalid("An entry has an unsafe file name."));
    }
    Ok(())
}
fn safe_local_name(name: &str) -> Result<(), CommandError> {
    safe_name(name)?;
    if cfg!(windows) {
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if name.contains([':', '<', '>', '"', '|', '?', '*'])
            || name.ends_with(['.', ' '])
            || ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(invalid(
                "A file name is not supported on Windows. Rename it before transferring.",
            ));
        }
    }
    Ok(())
}
fn valid_path(path: &str) -> Result<(), CommandError> {
    if path.is_empty() || path.len() > 32768 || path.contains('\0') {
        return Err(invalid("Enter a valid file path."));
    }
    Ok(())
}
fn remote_join(base: &str, name: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), name)
}
fn local_display(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    value.strip_prefix(r"\\?\").unwrap_or(&value).into()
}
fn time_ms(value: std::io::Result<std::time::SystemTime>) -> Option<u64> {
    value
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

impl Services {
    fn sftp_connection(&self, id: &str) -> Result<Arc<SftpConnection>, CommandError> {
        state_lock(&self.network.sftp)?
            .get(id)
            .filter(|s| !s.stop.is_cancelled() && !s.session.is_closed())
            .cloned()
            .ok_or_else(|| invalid("SFTP is disconnected. Connect to the server again."))
    }
    pub async fn sftp_connect(&self, host_id: &str) -> Result<SftpView, CommandError> {
        if state_lock(&self.network.sftp)?.len() >= 8 {
            return Err(invalid(
                "Close an SFTP connection before opening another (limit 8).",
            ));
        }
        let session = self.network_connect(host_id).await?;
        let client = tokio::time::timeout(Duration::from_secs(20), session.open_sftp())
            .await
            .map_err(|_| invalid("SFTP subsystem timed out."))?
            .map_err(failed)?;
        let home = client.canonicalize(".").await.map_err(failed)?;
        let id = HostId::new().to_string();
        let connection = Arc::new(SftpConnection {
            session,
            client: Arc::new(client),
            // Keep the established SFTP session alive through a vault lock.
            stop: CancellationToken::new(),
            host_id: host_id.into(),
            home: home.clone(),
        });
        let rejected = {
            let mut sessions = state_lock(&self.network.sftp)?;
            if self.agent.drain.is_cancelled() {
                Some(invalid("The vault is locking."))
            } else if sessions.len() >= 8 {
                Some(invalid(
                    "Close an SFTP connection before opening another (limit 8).",
                ))
            } else {
                sessions.insert(id.clone(), connection.clone());
                None
            }
        };
        if let Some(error) = rejected {
            let _ = tokio::time::timeout(Duration::from_secs(2), connection.session.close()).await;
            return Err(error);
        }
        Ok(SftpView {
            id,
            host_id: host_id.into(),
            home,
        })
    }
    pub fn sftp_list(&self) -> Result<Vec<SftpView>, CommandError> {
        Ok(state_lock(&self.network.sftp)?
            .iter()
            .filter(|(_, s)| !s.stop.is_cancelled() && !s.session.is_closed())
            .map(|(id, s)| SftpView {
                id: id.clone(),
                host_id: s.host_id.clone(),
                home: s.home.clone(),
            })
            .collect())
    }
    pub async fn sftp_disconnect(&self, id: &str) -> Result<(), CommandError> {
        let session = state_lock(&self.network.sftp)?.remove(id);
        if let Some(session) = session {
            session.stop.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(2), session.client.close()).await;
            let _ = tokio::time::timeout(Duration::from_secs(2), session.session.close()).await;
        }
        Ok(())
    }
    pub async fn files_list(
        &self,
        side: &str,
        connection_id: Option<&str>,
        path: &str,
    ) -> Result<DirectoryView, CommandError> {
        if side == "local" {
            local_list(path).await
        } else if side == "remote" {
            let connection = self.sftp_connection(connection_id.unwrap_or_default())?;
            valid_path(path)?;
            let path = connection.client.canonicalize(path).await.map_err(failed)?;
            let entries = remote_list(&connection, &path).await?;
            let parent = if path == "/" {
                None
            } else {
                Some(
                    path.rsplit_once('/')
                        .map(|(p, _)| if p.is_empty() { "/" } else { p })
                        .unwrap_or("/")
                        .into(),
                )
            };
            Ok(DirectoryView {
                path,
                parent,
                entries,
            })
        } else {
            Err(invalid("Choose local or remote files."))
        }
    }
    pub async fn files_action(
        &self,
        side: &str,
        connection_id: Option<&str>,
        action: &str,
        path: &str,
        new_name: Option<&str>,
    ) -> Result<(), CommandError> {
        valid_path(path)?;
        if let Some(name) = new_name {
            safe_name(name)?;
        }
        if side == "local" {
            if let Some(name) = new_name {
                safe_local_name(name)?;
            }
        }
        match side {
            "local" => match action {
                "mkdir" => fs::create_dir(
                    Path::new(path)
                        .join(new_name.ok_or_else(|| invalid("Folder name is required."))?),
                )
                .await
                .map_err(failed),
                "rename" => {
                    let source = Path::new(path);
                    let destination = source
                        .parent()
                        .ok_or_else(|| invalid("Cannot rename a root."))?
                        .join(new_name.ok_or_else(|| invalid("New name is required."))?);
                    if fs::try_exists(&destination).await.map_err(failed)? {
                        return Err(invalid("A file with that name already exists."));
                    }
                    fs::rename(source, destination).await.map_err(failed)
                }
                "delete" => {
                    let meta = fs::symlink_metadata(path).await.map_err(failed)?;
                    if meta.is_dir() {
                        fs::remove_dir(path).await.map_err(failed)
                    } else {
                        fs::remove_file(path).await.map_err(failed)
                    }
                }
                _ => Err(invalid("Unknown file operation.")),
            },
            "remote" => {
                let s = self.sftp_connection(connection_id.unwrap_or_default())?;
                match action {
                    "mkdir" => s
                        .client
                        .create_dir(remote_join(
                            path,
                            new_name.ok_or_else(|| invalid("Folder name is required."))?,
                        ))
                        .await
                        .map_err(failed),
                    "rename" => {
                        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or(".");
                        let destination = remote_join(
                            parent,
                            new_name.ok_or_else(|| invalid("New name is required."))?,
                        );
                        if s.client.try_exists(&destination).await.map_err(failed)? {
                            return Err(invalid("A file with that name already exists."));
                        }
                        s.client.rename(path, destination).await.map_err(failed)
                    }
                    "delete" => {
                        let meta = s.client.symlink_metadata(path).await.map_err(failed)?;
                        if meta.is_dir() {
                            s.client.remove_dir(path).await.map_err(failed)
                        } else {
                            s.client.remove_file(path).await.map_err(failed)
                        }
                    }
                    _ => Err(invalid("Unknown file operation.")),
                }
            }
            _ => Err(invalid("Choose local or remote files.")),
        }
    }
    pub fn transfer_watch(&self, sink: TransferSink) -> Result<Vec<TransferView>, CommandError> {
        {
            let jobs = state_lock(&self.network.transfers)?;
            for job in jobs.values() {
                *state_lock(&job.event)? = sink.clone();
            }
        }
        self.transfer_list()
    }
    pub fn transfer_list(&self) -> Result<Vec<TransferView>, CommandError> {
        state_lock(&self.network.transfers)?
            .values()
            .map(|j| Ok(state_lock(&j.view)?.clone()))
            .collect()
    }
    pub fn transfer_clear(&self) -> Result<Vec<String>, CommandError> {
        let mut jobs = state_lock(&self.network.transfers)?;
        let mut cleared = Vec::new();
        for (id, job) in jobs.iter() {
            let view = state_lock(&job.view)?;
            if matches!(view.status.as_str(), "completed" | "failed" | "cancelled") {
                cleared.push(id.clone());
            }
        }
        for id in &cleared {
            jobs.remove(id);
        }
        Ok(cleared)
    }
    pub fn transfer_cancel(&self, id: &str) -> Result<(), CommandError> {
        if let Some(job) = state_lock(&self.network.transfers)?.get(id) {
            job.stop.cancel();
        }
        Ok(())
    }
    pub fn transfer_start(
        &self,
        input: TransferInput,
        sink: TransferSink,
    ) -> Result<TransferView, CommandError> {
        if !["upload", "download"].contains(&input.direction.as_str())
            || input.sources.is_empty()
            || input.sources.len() > 256
        {
            return Err(invalid(
                "Choose up to 256 files or folders to upload or download.",
            ));
        }
        valid_path(&input.destination)?;
        for path in &input.sources {
            valid_path(path)?;
        }
        let connection = self.sftp_connection(&input.connection_id)?;
        let registry = self.network.clone();
        let id = HostId::new().to_string();
        let initial = TransferView {
            id: id.clone(),
            connection_id: input.connection_id.clone(),
            direction: input.direction.clone(),
            status: "queued".into(),
            current_file: String::new(),
            destination: input.destination.clone(),
            total_bytes: 0,
            transferred_bytes: 0,
            total_files: 0,
            completed_files: 0,
            skipped_files: 0,
            bytes_per_second: 0,
            error: None,
        };
        let job = Arc::new(TransferJob {
            stop: connection.stop.child_token(),
            view: Mutex::new(initial.clone()),
            event: Mutex::new(sink),
        });
        {
            let mut jobs = state_lock(&registry.transfers)?;
            if jobs.len() >= 100 {
                jobs.retain(|_, j| {
                    j.view
                        .lock()
                        .map(|v| !["completed", "failed", "cancelled"].contains(&v.status.as_str()))
                        .unwrap_or(false)
                });
            }
            if jobs
                .values()
                .filter(|j| {
                    j.view
                        .lock()
                        .map(|v| {
                            ["queued", "scanning", "transferring"].contains(&v.status.as_str())
                        })
                        .unwrap_or(false)
                })
                .count()
                >= 32
            {
                return Err(invalid("The transfer queue is full (32 jobs)."));
            }
            jobs.insert(id, job.clone());
        }
        let relay = job.clone();
        let sink: TransferSink = Arc::new(move |view| {
            if let Ok(event) = relay.event.lock() {
                event(view);
            }
        });
        sink(initial.clone());
        job.clone().spawn(registry, sink.clone(), async move {
            run_transfer(&connection, &job, &input, &sink).await
        });
        Ok(initial)
    }
}

async fn local_list(input: &str) -> Result<DirectoryView, CommandError> {
    if input == "@drives" {
        let mut entries = Vec::new();
        if cfg!(windows) {
            for letter in b'A'..=b'Z' {
                let path = format!("{}:\\", letter as char);
                if fs::try_exists(&path).await.unwrap_or(false) {
                    entries.push(FileEntry {
                        name: path.clone(),
                        path,
                        kind: "directory".into(),
                        size: 0,
                        modified_ms: None,
                        permissions: None,
                    });
                }
            }
        } else {
            entries.push(FileEntry {
                name: "/".into(),
                path: "/".into(),
                kind: "directory".into(),
                size: 0,
                modified_ms: None,
                permissions: None,
            });
        }
        return Ok(DirectoryView {
            path: input.into(),
            parent: None,
            entries,
        });
    }
    let path = if input.is_empty() {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))
    } else {
        valid_path(input)?;
        PathBuf::from(input)
    };
    let path = fs::canonicalize(path).await.map_err(failed)?;
    let mut reader = fs::read_dir(&path).await.map_err(failed)?;
    let mut entries = Vec::new();
    let mut budget = MetadataBudget::new(DIRECTORY_LIMIT, 8 * 1024 * 1024);
    while let Some(entry) = reader.next_entry().await.map_err(failed)? {
        if entries.len() >= DIRECTORY_LIMIT {
            return Err(invalid(
                "This folder has more than 20,000 entries. Open a smaller subfolder.",
            ));
        }
        let file_type = entry.file_type().await.map_err(failed)?;
        let meta = entry.metadata().await.ok();
        let item = FileEntry {
            name: entry.file_name().to_string_lossy().into(),
            path: local_display(&entry.path()),
            kind: if file_type.is_symlink() {
                "symlink"
            } else if file_type.is_dir() {
                "directory"
            } else if file_type.is_file() {
                "file"
            } else {
                "other"
            }
            .into(),
            size: meta.as_ref().map_or(0, |m| m.len()),
            modified_ms: meta.and_then(|m| time_ms(m.modified())),
            permissions: None,
        };
        budget.claim(
            std::mem::size_of::<FileEntry>() + item.name.len() + item.path.len() + item.kind.len(),
        )?;
        entries.push(item);
    }
    sort_entries(&mut entries);
    let parent = path
        .parent()
        .map(local_display)
        .or_else(|| Some("@drives".into()));
    Ok(DirectoryView {
        path: local_display(&path),
        parent,
        entries,
    })
}
fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        (a.kind != "directory")
            .cmp(&(b.kind != "directory"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}
async fn remote_list(
    connection: &SftpConnection,
    path: &str,
) -> Result<Vec<FileEntry>, CommandError> {
    let stream = connection.session.sftp_stream().await.map_err(failed)?;
    let raw = RawSftpSession::new(stream);
    raw.init().await.map_err(failed)?;
    let handle = raw.opendir(path).await.map_err(failed)?.handle;
    let mut entries = Vec::new();
    let mut budget = MetadataBudget::new(DIRECTORY_LIMIT, 8 * 1024 * 1024);
    let result=async {loop{let batch=tokio::select!{biased;_=connection.stop.cancelled()=>return Err(invalid("SFTP disconnected.")),batch=raw.readdir(&handle)=>batch};match batch {
        Ok(batch)=>for file in batch.files{if [".",".."].contains(&file.filename.as_str()){continue;}safe_name(&file.filename)?;if entries.len()>=DIRECTORY_LIMIT{return Err(invalid("This folder has more than 20,000 entries. Open a smaller subfolder."));}let attrs=file.attrs;let entry=FileEntry{name:file.filename.clone(),path:remote_join(path,&file.filename),kind:if attrs.is_symlink(){"symlink"}else if attrs.is_dir(){"directory"}else if attrs.is_regular(){"file"}else{"other"}.into(),size:attrs.size.unwrap_or(0),modified_ms:attrs.mtime.map(|t|u64::from(t)*1000),permissions:attrs.permissions};budget.claim(std::mem::size_of::<FileEntry>()+entry.name.len()+entry.path.len()+entry.kind.len())?;entries.push(entry);},
        Err(SftpError::Status(status)) if status.status_code==StatusCode::Eof=>break,
        Err(e)=>return Err(failed(e))
    }}Ok(())}.await;
    let _ = raw.close(handle).await;
    let _ = raw.close_session();
    result?;
    sort_entries(&mut entries);
    Ok(entries)
}

struct CopyItem {
    source: String,
    destination: String,
    directory: bool,
    size: u64,
}
fn update(job: &TransferJob, sink: &TransferSink, apply: impl FnOnce(&mut TransferView)) {
    if let Ok(mut v) = job.view.lock() {
        apply(&mut v);
        sink(v.clone());
    }
}
async fn run_transfer(
    connection: &SftpConnection,
    job: &TransferJob,
    input: &TransferInput,
    sink: &TransferSink,
) -> Result<(), CommandError> {
    update(job, sink, |v| v.status = "scanning".into());
    let upload = input.direction == "upload";
    let destination = if upload {
        connection
            .client
            .canonicalize(&input.destination)
            .await
            .map_err(failed)?
    } else {
        local_display(&fs::canonicalize(&input.destination).await.map_err(failed)?)
    };
    if upload {
        if !connection
            .client
            .metadata(&destination)
            .await
            .map_err(failed)?
            .is_dir()
        {
            return Err(invalid("Destination must be a folder."));
        }
    } else if !fs::metadata(&destination).await.map_err(failed)?.is_dir() {
        return Err(invalid("Destination must be a folder."));
    }
    let mut pending = Vec::new();
    let mut metadata = MetadataBudget::new(PLAN_LIMIT, 32 * 1024 * 1024);
    for source in &input.sources {
        let basename = if upload {
            Path::new(source).file_name().and_then(|n| n.to_str())
        } else {
            source.trim_end_matches('/').rsplit('/').next()
        }
        .ok_or_else(|| invalid("Choose a file or folder, not a root."))?;
        safe_name(basename)?;
        let target = if upload {
            remote_join(&destination, basename)
        } else {
            safe_local_name(basename)?;
            local_display(&Path::new(&destination).join(basename))
        };
        metadata.claim(std::mem::size_of::<CopyItem>() + source.len() + target.len())?;
        pending.push((source.clone(), target));
    }
    let mut plan = Vec::new();
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut skipped = 0u64;
    while let Some((source, target)) = pending.pop() {
        if job.stop.is_cancelled() {
            return Err(invalid("Transfer cancelled."));
        }
        if plan.len() + pending.len() >= PLAN_LIMIT {
            return Err(invalid(
                "Transfer contains more than 100,000 entries. Split it into smaller jobs.",
            ));
        }
        let (directory, link, size) = if upload {
            let meta = fs::symlink_metadata(&source).await.map_err(failed)?;
            if !meta.is_dir() && !meta.is_file() && !meta.is_symlink() {
                return Err(invalid(
                    "Only regular files and folders can be transferred.",
                ));
            }
            (meta.is_dir(), meta.is_symlink(), meta.len())
        } else {
            let meta = connection
                .client
                .symlink_metadata(&source)
                .await
                .map_err(failed)?;
            if !meta.is_dir() && !meta.is_regular() && !meta.is_symlink() {
                return Err(invalid(
                    "Only regular files and folders can be transferred.",
                ));
            }
            (meta.is_dir(), meta.is_symlink(), meta.size.unwrap_or(0))
        };
        if link {
            skipped += 1;
            continue;
        }
        if directory {
            let listing = async {
                if upload {
                    Ok(local_list(&source).await?.entries)
                } else {
                    remote_list(connection, &source).await
                }
            };
            let entries = tokio::select! {biased;_=job.stop.cancelled()=>return Err(invalid("Transfer cancelled.")),entries=listing=>entries?};
            for entry in entries {
                safe_name(&entry.name)?;
                let target = if upload {
                    remote_join(&target, &entry.name)
                } else {
                    safe_local_name(&entry.name)?;
                    local_display(&Path::new(&target).join(&entry.name))
                };
                metadata
                    .claim(std::mem::size_of::<CopyItem>() + entry.path.len() + target.len())?;
                pending.push((entry.path, target));
            }
        } else {
            bytes = bytes
                .checked_add(size)
                .ok_or_else(|| invalid("Transfer is too large."))?;
            files += 1;
        }
        plan.push(CopyItem {
            source,
            destination: target,
            directory,
            size,
        });
    }
    update(job, sink, |v| {
        v.status = "transferring".into();
        v.total_bytes = bytes;
        v.total_files = files;
        v.skipped_files = skipped;
    });
    let began = Instant::now();
    let mut last = Instant::now();
    for item in plan {
        if job.stop.is_cancelled() {
            return Err(invalid("Transfer cancelled."));
        }
        if let Ok(mut v) = job.view.lock() {
            v.current_file = item.source.clone();
        }
        if item.directory {
            if upload {
                if connection
                    .client
                    .try_exists(&item.destination)
                    .await
                    .map_err(failed)?
                {
                    if !connection
                        .client
                        .symlink_metadata(&item.destination)
                        .await
                        .map_err(failed)?
                        .is_dir()
                    {
                        return Err(invalid(
                            "A destination folder conflicts with a file or symlink.",
                        ));
                    }
                } else {
                    connection
                        .client
                        .create_dir(&item.destination)
                        .await
                        .map_err(failed)?;
                }
            } else if fs::try_exists(&item.destination).await.map_err(failed)? {
                let meta = fs::symlink_metadata(&item.destination)
                    .await
                    .map_err(failed)?;
                if meta.is_symlink() || !meta.is_dir() {
                    return Err(invalid(
                        "A destination folder conflicts with a file or symlink.",
                    ));
                }
            } else {
                fs::create_dir(&item.destination).await.map_err(failed)?;
            }
            continue;
        }
        let exists = if upload {
            connection
                .client
                .try_exists(&item.destination)
                .await
                .map_err(failed)?
        } else {
            fs::try_exists(&item.destination).await.map_err(failed)?
        };
        if exists && !input.overwrite {
            if let Ok(mut v) = job.view.lock() {
                v.skipped_files += 1;
                v.completed_files += 1;
                v.total_bytes = v.total_bytes.saturating_sub(item.size);
                if last.elapsed() >= Duration::from_millis(100) {
                    sink(v.clone());
                    last = Instant::now();
                }
            }
            continue;
        }
        copy_file(connection, job, &item, input, sink, began, &mut last).await?;
        if let Ok(mut v) = job.view.lock() {
            v.completed_files += 1;
            if last.elapsed() >= Duration::from_millis(100) {
                sink(v.clone());
                last = Instant::now();
            }
        }
    }
    Ok(())
}

async fn copy_file(
    connection: &SftpConnection,
    job: &TransferJob,
    item: &CopyItem,
    input: &TransferInput,
    sink: &TransferSink,
    began: Instant,
    last: &mut Instant,
) -> Result<(), CommandError> {
    let upload = input.direction == "upload";
    let overwrite = input.overwrite;
    if !upload {
        let parent = Path::new(&item.destination)
            .parent()
            .ok_or_else(|| invalid("Invalid destination folder."))?
            .to_path_buf();
        let required = item.size;
        tokio::task::spawn_blocking(move || check_space(&parent, required))
            .await
            .map_err(failed)??;
    }
    let suffix = HostId::new().to_string();
    let sibling = |name: &str| {
        if upload {
            remote_join(
                item.destination
                    .rsplit_once('/')
                    .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
                    .unwrap_or("."),
                name,
            )
        } else {
            local_display(
                &Path::new(&item.destination)
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(name),
            )
        }
    };
    let temp = sibling(&format!(".selai-part-{suffix}"));
    let mut progress = |n: usize| {
        if let Ok(mut view) = job.view.lock() {
            view.transferred_bytes += n as u64;
            view.bytes_per_second =
                (view.transferred_bytes as f64 / began.elapsed().as_secs_f64().max(0.01)) as u64;
            if last.elapsed() >= Duration::from_millis(100) {
                sink(view.clone());
                *last = Instant::now();
            }
        }
    };
    let result = async {
        if upload {
            let mut source = fs::File::open(&item.source).await.map_err(failed)?;
            let mut destination = connection
                .client
                .open_with_flags(
                    &temp,
                    OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                )
                .await
                .map_err(failed)?;
            copy_bytes(
                &mut source,
                &mut destination,
                item.size,
                &job.stop,
                &mut progress,
            )
            .await?;
            destination.shutdown().await.map_err(failed)?;
            if job.stop.is_cancelled() {
                return Err(invalid("Transfer cancelled."));
            }
            if overwrite
                && connection
                    .client
                    .try_exists(&item.destination)
                    .await
                    .map_err(failed)?
            {
                let attrs = connection
                    .client
                    .symlink_metadata(&item.destination)
                    .await
                    .map_err(failed)?;
                if !attrs.is_regular() {
                    return Err(invalid("Only regular destination files can be replaced."));
                }
                let backup = sibling(&format!(".selai-backup-{suffix}"));
                connection
                    .client
                    .rename(&item.destination, &backup)
                    .await
                    .map_err(failed)?;
                if let Err(error) = connection.client.rename(&temp, &item.destination).await {
                    let _ = connection.client.rename(&backup, &item.destination).await;
                    return Err(failed(error));
                }
                connection
                    .client
                    .remove_file(backup)
                    .await
                    .map_err(failed)?;
            } else {
                connection
                    .client
                    .rename(&temp, &item.destination)
                    .await
                    .map_err(failed)?;
            }
        } else {
            let mut source = connection.client.open(&item.source).await.map_err(failed)?;
            let mut destination = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .await
                .map_err(failed)?;
            copy_bytes(
                &mut source,
                &mut destination,
                item.size,
                &job.stop,
                &mut progress,
            )
            .await?;
            destination.flush().await.map_err(failed)?;
            destination.sync_all().await.map_err(failed)?;
            drop(destination);
            if job.stop.is_cancelled() {
                return Err(invalid("Transfer cancelled."));
            }
            if overwrite {
                if let Ok(meta) = fs::symlink_metadata(&item.destination).await {
                    if meta.is_symlink() || !meta.is_file() {
                        return Err(invalid("Only regular destination files can be replaced."));
                    }
                }
            }
            // Native no-replace rename also works on Windows FAT/exFAT.
            let source_path = temp.clone();
            let target_path = item.destination.clone();
            tokio::task::spawn_blocking(move || {
                let path = tempfile::TempPath::try_from_path(source_path).map_err(failed)?;
                if overwrite {
                    path.persist(target_path).map_err(failed)
                } else {
                    path.persist_noclobber(target_path).map_err(failed)
                }
            })
            .await
            .map_err(failed)??;
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        if upload {
            let _ = connection.client.remove_file(&temp).await;
        } else {
            let _ = fs::remove_file(&temp).await;
        }
    }
    result
}

async fn copy_bytes<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    source: &mut R,
    destination: &mut W,
    expected: u64,
    cancel: &CancellationToken,
    mut progress: impl FnMut(usize),
) -> Result<(), CommandError> {
    let mut buffer = vec![0; 64 * 1024];
    let mut copied = 0u64;
    loop {
        let n = tokio::select! {biased;_=cancel.cancelled()=>return Err(invalid("Transfer cancelled.")),result=source.read(&mut buffer)=>result.map_err(failed)?};
        if n == 0 {
            break;
        }
        copied = copied
            .checked_add(n as u64)
            .ok_or_else(|| invalid("Transfer is too large."))?;
        if copied > expected {
            return Err(invalid(
                "Source size changed during transfer. Retry when the file is stable.",
            ));
        }
        tokio::select! {biased;_=cancel.cancelled()=>return Err(invalid("Transfer cancelled.")),result=destination.write_all(&buffer[..n])=>result.map_err(failed)?};
        progress(n);
    }
    if copied != expected {
        return Err(invalid(
            "Source size changed during transfer. Retry when the file is stable.",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn queued_job(direction: &str) -> Arc<TransferJob> {
        Arc::new(TransferJob {
            stop: CancellationToken::new(),
            view: Mutex::new(TransferView {
                id: "test-transfer".into(),
                connection_id: "test-connection".into(),
                direction: direction.into(),
                status: "queued".into(),
                current_file: String::new(),
                destination: String::new(),
                total_bytes: 0,
                transferred_bytes: 0,
                total_files: 0,
                completed_files: 0,
                skipped_files: 0,
                bytes_per_second: 0,
                error: None,
            }),
            event: Mutex::new(Arc::new(|_| {})),
        })
    }

    #[test]
    fn clearing_transfer_history_preserves_active_jobs() {
        let services = Services::in_memory(Arc::new(selai_secrets::MemoryStore::new())).unwrap();
        for status in [
            "queued",
            "scanning",
            "transferring",
            "completed",
            "failed",
            "cancelled",
        ] {
            let job = queued_job("upload");
            {
                let mut view = job.view.lock().unwrap();
                view.id = status.into();
                view.status = status.into();
            }
            services
                .network
                .transfers
                .lock()
                .unwrap()
                .insert(status.into(), job);
        }
        let mut cleared = services.transfer_clear().unwrap();
        cleared.sort();
        assert_eq!(cleared, ["cancelled", "completed", "failed"]);
        let remaining = services.transfer_list().unwrap();
        assert_eq!(remaining.len(), 3);
        for job in services.network.transfers.lock().unwrap().values() {
            assert!(!job.stop.is_cancelled());
            assert!(matches!(
                job.view.lock().unwrap().status.as_str(),
                "queued" | "scanning" | "transferring"
            ));
        }
        assert!(services.transfer_clear().unwrap().is_empty());
    }

    // Unlike #[tokio::test], this reproduces the synchronous WebView caller:
    // neither the caller nor the spawned OS thread has an entered runtime.
    #[test]
    fn transfers_start_without_an_entered_tokio_runtime() {
        for direction in ["upload", "download"] {
            let job = queued_job(direction);
            let worker = job.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let registry = Arc::new(NetworkRegistry::default());
            let slots = registry.clone();
            std::thread::spawn(move || {
                assert!(tokio::runtime::Handle::try_current().is_err());
                let sink: TransferSink = Arc::new(move |view| {
                    tx.send(view).unwrap();
                });
                job.spawn(registry, sink, async move {
                    let dir = tempfile::tempdir().unwrap();
                    let source = dir.path().join("source.bin");
                    let destination = dir.path().join("destination.bin");
                    let payload = vec![42; 3 * 64 * 1024 + 17];
                    fs::write(&source, &payload).await.map_err(failed)?;
                    let mut reader = fs::File::open(source).await.map_err(failed)?;
                    let mut writer = fs::File::create(&destination).await.map_err(failed)?;
                    copy_bytes(
                        &mut reader,
                        &mut writer,
                        payload.len() as u64,
                        &worker.stop,
                        |n| {
                            worker.view.lock().unwrap().transferred_bytes += n as u64;
                        },
                    )
                    .await?;
                    writer.shutdown().await.map_err(failed)?;
                    assert_eq!(fs::read(destination).await.map_err(failed)?, payload);
                    Ok(())
                });
            })
            .join()
            .expect("transfer submission must not panic on a sync thread");
            let finished = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("terminal event");
            assert_eq!(finished.status, "completed", "{:?}", finished.error);
            assert_eq!(finished.direction, direction);
            assert_eq!(finished.transferred_bytes, 3 * 64 * 1024 + 17);
            assert_eq!(slots.transfer_slots.available_permits(), 2);
        }
    }

    #[test]
    fn queued_transfer_cancellation_and_copy_failure_report_terminal_events() {
        for cancelled in [true, false] {
            let job = queued_job("upload");
            let registry = Arc::new(NetworkRegistry::default());
            // Fill both slots so cancellation must work before a job can run.
            let held = registry.transfer_slots.try_acquire_many(2).unwrap();
            if cancelled {
                job.stop.cancel();
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let sink: TransferSink = Arc::new(move |view| {
                tx.send(view).unwrap();
            });
            job.spawn(registry.clone(), sink, async {
                Err(invalid("Copy failed."))
            });
            if !cancelled {
                drop(held);
            }
            let finished = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("terminal event");
            assert_eq!(
                finished.status,
                if cancelled { "cancelled" } else { "failed" }
            );
            assert_eq!(
                finished.error.as_deref(),
                if cancelled {
                    None
                } else {
                    Some("Copy failed.")
                }
            );
        }
    }

    #[test]
    fn transfer_names_cannot_escape_the_destination() {
        for name in [".", "..", "../outside", "a/b", r"a\b", ""] {
            assert!(safe_name(name).is_err());
        }
        assert!(safe_name("report-2026.txt").is_ok());
        assert_eq!(
            local_display(Path::new(r"\\?\UNC\server\share\folder")),
            r"\\server\share\folder"
        );
        if cfg!(windows) {
            for name in [
                "C:outside",
                "CON",
                "NUL.txt",
                "COM1.txt",
                "data:stream",
                "trailing.",
            ] {
                assert!(safe_local_name(name).is_err(), "{name}");
            }
        }
    }
}
