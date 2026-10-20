//! Thin Tauri command layer. Each function forwards to `service::Services`;
//! no decisions are made here.

use std::sync::Arc;

use selai_domain::{ErrorCode, Page};
use tauri::{ipc::Channel, State};

use crate::{
    agent_service::{
        AgentEventView, AuditVerifyView, ExecutionView, SudoAnswerInput, SudoOptionsView,
    },
    chat_service::{ChatEventView, ChatMessageInput},
    chat_sessions::{ChatSessionInput, ChatSessionView},
    dto::{
        AppInfo, CommandError, GroupView, HostInput, HostView, ModelsView, ProbeView,
        ProviderInput, ProviderModelsInput, ProviderTestView, ProviderView, StorageStatus,
        TemplateView, TerminalEvent, TrustMode, TrustView,
    },
    history_service::{
        AiPreferences, AiPreferencesInput, ExportResult, HistoryFilterInput, HostUsageView,
        PurgeResult, TaskView,
    },
    service::{EventSink, Services},
    AppState,
};

fn services(
    state: &State<'_, AppState>,
) -> Result<crate::vault_service::ServiceLease, CommandError> {
    state.services()
}

#[tauri::command]
pub async fn vault_status(
    state: State<'_, AppState>,
) -> Result<crate::vault_service::VaultStatus, CommandError> {
    let manager = state.vault()?;
    tauri::async_runtime::spawn_blocking(move || manager.status())
        .await
        .map_err(|_| CommandError::new(selai_domain::ErrorCode::Internal, "Vault status failed."))?
}
#[tauri::command]
pub async fn vault_watch(
    state: State<'_, AppState>,
    on_event: Channel<crate::vault_service::VaultStatus>,
) -> Result<crate::vault_service::VaultStatus, CommandError> {
    let manager = state.vault()?;
    tauri::async_runtime::spawn_blocking(move || {
        manager.watch_status(Arc::new(move |status| {
            let _ = on_event.send(status);
        }))
    })
    .await
    .map_err(|_| CommandError::new(ErrorCode::Internal, "Vault status subscription failed."))?
}
#[tauri::command]
pub fn vault_flush_complete(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.vault()?.flush_complete()
}
#[tauri::command]
pub fn vault_activity(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.vault()?.activity()
}
#[tauri::command]
pub fn vault_lock(state: State<'_, AppState>, force: bool) -> Result<(), CommandError> {
    let manager = state.vault()?;
    tauri::async_runtime::spawn(async move {
        let _ = manager.lock(force).await;
    });
    Ok(())
}

/// A native picker can remain open while the vault locks. Its worker captures
/// no passphrase or data key; cancellation drops the caller's secret buffers.
async fn vault_file_dialog(
    manager: &Arc<crate::vault_service::VaultManager>,
    title: &'static str,
    extension: &'static str,
    filename: Option<&'static str>,
    gate: bool,
) -> Result<Option<std::path::PathBuf>, CommandError> {
    let drain = if gate {
        Some(manager.services()?.agent.drain.clone())
    } else {
        None
    };
    let dialog = tauri::async_runtime::spawn_blocking(move || {
        let picker = rfd::FileDialog::new()
            .set_title(title)
            .add_filter("Selai file", &[extension]);
        match filename {
            Some(name) => picker.set_file_name(name).save_file(),
            None => picker.pick_file(),
        }
    });
    let result = if let Some(token) = drain {
        tokio::select! {biased;_=token.cancelled()=>return Err(CommandError::new(selai_domain::ErrorCode::SecretStoreLocked,"The vault locked. Choose the file again after unlocking.")),result=dialog=>result}
    } else {
        dialog.await
    };
    result.map_err(|_| CommandError::new(selai_domain::ErrorCode::Internal, "File picker failed."))
}

/// File paths come from native dialogs, never a webview-supplied path.
#[tauri::command]
pub async fn vault_action(
    state: State<'_, AppState>,
    action: String,
    passphrase: Option<String>,
    replacement: Option<String>,
    minutes: Option<u32>,
) -> Result<Option<String>, CommandError> {
    let manager = state.vault()?;
    let phrase = zeroize::Zeroizing::new(passphrase.unwrap_or_default());
    let replacement = zeroize::Zeroizing::new(replacement.unwrap_or_default());
    let mut source = None;
    let mut destination = None;
    match action.as_str() {
        "saveRecovery" => {
            destination = vault_file_dialog(
                &manager,
                "Save recovery key — keep it separate from your backup",
                "txt",
                Some("selai-recovery-key.txt"),
                true,
            )
            .await?;
            if destination.is_none() {
                return Ok(None);
            }
        }
        "backup" => {
            destination = vault_file_dialog(
                &manager,
                "Save encrypted vault backup",
                "selai",
                Some("selai-backup.selai"),
                true,
            )
            .await?;
            if destination.is_none() {
                return Ok(None);
            }
        }
        "restore" | "restoreRecovery" => {
            source = vault_file_dialog(
                &manager,
                "Restore encrypted vault backup",
                "selai",
                None,
                false,
            )
            .await?;
            if source.is_none() {
                return Ok(None);
            }
        }
        "exportArchive" => {
            destination = vault_file_dialog(
                &manager,
                "Save encrypted legacy history export",
                "selai-history",
                Some("selai-legacy-history.selai-history"),
                true,
            )
            .await?;
            if destination.is_none() {
                return Ok(None);
            }
        }
        "decryptHistory" => {
            source = vault_file_dialog(
                &manager,
                "Open encrypted history export",
                "selai-history",
                None,
                true,
            )
            .await?;
            if source.is_none() {
                return Ok(None);
            }
            destination = vault_file_dialog(
                &manager,
                "Save decrypted history — this file is NOT encrypted",
                "json",
                Some("selai-history.json"),
                true,
            )
            .await?;
            if destination.is_none() {
                return Ok(None);
            }
        }
        _ => (),
    }
    tauri::async_runtime::spawn_blocking(move || {
        match action.as_str() {
            "saveRecovery"=>{
                let Some(path)=destination else{return Ok(None)};
                let _lease=manager.services()?;
                manager.export_destination(&path)?;
                if phrase.len()!=43 || !phrase.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_'){return Err(CommandError::new(selai_domain::ErrorCode::InvalidInput,"Invalid recovery key."));}
                let document=zeroize::Zeroizing::new(format!("Selai recovery key\n{}\nKeep this key separate from your encrypted backup.\n",phrase.as_str()));
                selai_vault::write_export(&path,document.as_bytes()).map_err(|e|CommandError::new(selai_domain::ErrorCode::StorageIo,e.to_string()))?;
                Ok(Some(path.display().to_string()))
            },
            "setup" => manager.initialize(phrase,&selai_secrets::KeyringStore::new()).map(Some),
            "unlock" => manager.unlock(phrase).map(|_|None),
            "recover" => manager.recover(phrase,replacement).map(Some),
            "change" => manager.change(phrase,replacement).map(|_|None),
            "rotateRecovery" => manager.rotate(phrase).map(Some),
            "timeout" => manager.timeout(minutes.unwrap_or(15)).map(|_|None),
            "storageStatus" => manager.storage_usage().map(Some),
            "compact" => manager.compact().map(Some),
            "cleanup" => manager.retry_cleanup().map(|ok|Some(if ok {"Cleanup complete"}else{"Some legacy files or credentials could not be removed. Close older Selai instances, then retry."}.into())),
            "archives" => manager.archives().and_then(|names|serde_json::to_string(&names).map(Some).map_err(|e|CommandError::new(selai_domain::ErrorCode::Internal,e.to_string()))),
            "exportArchive" => {
                let Some(path)=destination else{return Ok(None)};
                manager.export_archive(&replacement,&path).map(|_|Some(path.display().to_string()))
            },
            "backup" => {
                let Some(path)=destination else{return Ok(None)};
                manager.backup(phrase,&path).map(|_|Some(path.display().to_string()))
            },
            "restore" | "restoreRecovery" => {
                let Some(path)=source else{return Ok(None)};
                if action=="restoreRecovery"{manager.restore_recovery(phrase,replacement,&path).map(Some)}else{manager.restore(phrase,&path).map(|_|None)}
            },
            "decryptHistory" => {
                let Some(path)=source else{return Ok(None)};
                let Some(out)=destination else{return Ok(None)};
                let _lease=manager.services()?;
                manager.export_destination(&out)?;
                let metadata=std::fs::metadata(&path).map_err(|e|CommandError::new(selai_domain::ErrorCode::StorageIo,e.to_string()))?;
                if metadata.len()>64*1024*1024{return Err(CommandError::new(selai_domain::ErrorCode::InvalidInput,"History export is too large."));}
                let bytes=std::fs::read(path).map_err(|e|CommandError::new(selai_domain::ErrorCode::StorageIo,e.to_string()))?;
                let plain=selai_vault::Vault::open_document(&bytes,&phrase).map_err(|e|CommandError::new(selai_domain::ErrorCode::InvalidInput,e.to_string()))?;
                selai_vault::write_export(&out,&plain).map_err(|e|CommandError::new(selai_domain::ErrorCode::StorageIo,e.to_string()))?;
                Ok(Some(out.display().to_string()))
            },
            _=>Err(CommandError::new(selai_domain::ErrorCode::InvalidInput,"Unknown vault action."))
        }
    }).await.map_err(|_|CommandError::new(selai_domain::ErrorCode::Internal,"Vault operation failed."))?
}

#[tauri::command]
pub fn app_info() -> AppInfo {
    Services::app_info()
}

#[tauri::command]
pub fn storage_status(state: State<'_, AppState>) -> Result<StorageStatus, CommandError> {
    services(&state)?.storage_status()
}

#[tauri::command]
pub fn host_list(
    state: State<'_, AppState>,
    query: Option<String>,
    include_archived: Option<bool>,
    cursor: Option<String>,
) -> Result<Page<HostView>, CommandError> {
    services(&state)?.host_list(query, include_archived.unwrap_or(false), cursor)
}

#[tauri::command]
pub fn local_host(state: State<'_, AppState>) -> Result<HostView, CommandError> {
    services(&state)?.local_host()
}

#[tauri::command]
pub fn host_get(state: State<'_, AppState>, id: String) -> Result<HostView, CommandError> {
    services(&state)?.host_get(&id)
}

#[tauri::command]
pub fn host_save(state: State<'_, AppState>, input: HostInput) -> Result<HostView, CommandError> {
    services(&state)?.host_save(input)
}

#[tauri::command]
pub fn host_move_group(
    state: State<'_, AppState>,
    id: String,
    group_id: Option<String>,
    expected_updated_at_ms: i64,
) -> Result<HostView, CommandError> {
    services(&state)?.host_move_group(&id, group_id.as_deref(), expected_updated_at_ms)
}

#[tauri::command]
pub fn ssh_key_list(
    state: State<'_, AppState>,
) -> Result<Vec<crate::dto::SshKeyView>, CommandError> {
    services(&state)?.ssh_key_list()
}

#[tauri::command]
pub fn ssh_key_import(
    state: State<'_, AppState>,
    name: String,
    private_key_base64: String,
    passphrase: Option<String>,
) -> Result<crate::dto::SshKeyView, CommandError> {
    services(&state)?.ssh_key_import(name, private_key_base64, passphrase)
}

#[tauri::command]
pub async fn ssh_key_import_from_file(
    state: State<'_, AppState>,
    name: String,
    passphrase: Option<String>,
) -> Result<Option<crate::dto::SshKeyView>, CommandError> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            CommandError::new(ErrorCode::InvalidInput, "home directory is unavailable")
        })?;
    let ssh_dir = home.join(".ssh");
    let path = tauri::async_runtime::spawn_blocking(move || {
        let picker = rfd::FileDialog::new()
            .set_title("Choose an SSH private key (files without extensions are supported)");
        let picker = if ssh_dir.is_dir() {
            picker.set_directory(ssh_dir)
        } else {
            picker
        };
        picker.pick_file()
    })
    .await
    .map_err(|_| CommandError::new(ErrorCode::Internal, "SSH key picker failed."))?;
    let Some(path) = path else {
        return Ok(None);
    };
    let bytes = crate::vault_service::read_key_file(&path)?;
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &*bytes);
    let key = services(&state)?.ssh_key_import(name, encoded, passphrase)?;
    Ok(Some(key))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshKeyScanResult {
    pub imported: Vec<crate::dto::SshKeyView>,
    pub already_saved: usize,
    pub skipped: usize,
}

/// Imports supported, unencrypted private keys in ~/.ssh, regardless of filename
/// extension. Public keys and SSH configuration files are ignored.
#[tauri::command]
pub fn ssh_key_scan_local(state: State<'_, AppState>) -> Result<SshKeyScanResult, CommandError> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            CommandError::new(ErrorCode::InvalidInput, "home directory is unavailable")
        })?;
    let dir = home.join(".ssh");
    let entries = std::fs::read_dir(&dir)
        .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "could not read ~/.ssh"))?;
    let lease = services(&state)?;
    let saved = lease.ssh_key_list()?;
    let mut fingerprints: std::collections::HashSet<String> = saved
        .iter()
        .map(|key| key.fingerprint_sha256.clone())
        .collect();
    let mut result = SshKeyScanResult {
        imported: Vec::new(),
        already_saved: 0,
        skipped: 0,
    };
    for entry in entries {
        let Ok(entry) = entry else {
            result.skipped += 1;
            continue;
        };
        let filename = entry.file_name().to_string_lossy().into_owned();
        if matches!(
            filename.as_str(),
            "config" | "known_hosts" | "known_hosts.old" | "authorized_keys"
        ) || filename.ends_with(".pub")
        {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            result.skipped += 1;
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let Ok(bytes) = crate::vault_service::read_key_file(&entry.path()) else {
            result.skipped += 1;
            continue;
        };
        let Ok(pem) = std::str::from_utf8(&bytes) else {
            result.skipped += 1;
            continue;
        };
        let Ok(parsed) = russh::keys::decode_secret_key(pem, None) else {
            result.skipped += 1;
            continue;
        };
        if selai_ssh::validate_private_key(&parsed).is_err() {
            result.skipped += 1;
            continue;
        }
        let fingerprint = parsed
            .public_key()
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string();
        if !fingerprints.insert(fingerprint) {
            result.already_saved += 1;
            continue;
        }
        if filename.trim().is_empty()
            || filename.len() > 80
            || filename.chars().any(char::is_control)
        {
            result.skipped += 1;
            continue;
        }
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&*bytes);
        result
            .imported
            .push(lease.ssh_key_import(filename, encoded, None)?);
    }
    Ok(result)
}

static SSH_KEY_GENERATION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

#[tauri::command]
pub async fn ssh_key_generate(
    state: State<'_, AppState>,
    name: String,
    algorithm: String,
) -> Result<crate::dto::SshKeyView, CommandError> {
    let lease = services(&state)?;
    let permit = SSH_KEY_GENERATION.try_acquire().map_err(|_| {
        CommandError::new(
            ErrorCode::StorageBusy,
            "SSH key generation is already running. Please wait.",
        )
    })?;
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        lease.ssh_key_generate(name, algorithm)
    })
    .await
    .map_err(|_| CommandError::new(ErrorCode::Internal, "SSH key generation failed."))?
}

#[tauri::command]
pub fn ssh_key_delete(state: State<'_, AppState>, id: String) -> Result<(), CommandError> {
    services(&state)?.ssh_key_delete(&id)
}

#[tauri::command]
pub fn kdbx_preview(
    state: State<'_, AppState>,
    file_base64: String,
    passphrase: String,
) -> Result<Vec<crate::kdbx_import::KdbxProfile>, CommandError> {
    let _lease = services(&state)?;
    crate::kdbx_import::preview(&file_base64, passphrase)
        .map_err(|message| CommandError::new(selai_domain::ErrorCode::InvalidInput, message))
}

#[tauri::command]
pub fn ssh_config_read() -> Result<String, CommandError> {
    use std::{io::Read, path::PathBuf};

    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).ok_or_else(|| {
            CommandError::new(ErrorCode::InvalidInput, "home directory is unavailable")
        })?;
    let path = PathBuf::from(home).join(".ssh").join("config");
    let file = std::fs::File::open(&path)
        .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "could not read ~/.ssh/config"))?;
    if file
        .metadata()
        .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "could not inspect ~/.ssh/config"))?
        .len()
        > 2 * 1024 * 1024
    {
        return Err(CommandError::new(
            ErrorCode::InvalidInput,
            "~/.ssh/config is larger than 2 MB",
        ));
    }
    let mut bytes = Vec::new();
    file.take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "could not read ~/.ssh/config"))?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(CommandError::new(
            ErrorCode::InvalidInput,
            "~/.ssh/config is larger than 2 MB",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "~/.ssh/config is not UTF-8"))
}

#[tauri::command]
pub fn host_archive(state: State<'_, AppState>, id: String) -> Result<(), CommandError> {
    services(&state)?.host_archive(&id)
}

#[tauri::command]
pub async fn host_probe(state: State<'_, AppState>, id: String) -> Result<ProbeView, CommandError> {
    let lease = services(&state)?;
    lease.run(lease.host_probe(&id)).await
}

#[tauri::command]
pub fn host_trust(
    state: State<'_, AppState>,
    id: String,
    fingerprint_sha256: String,
    mode: TrustMode,
) -> Result<TrustView, CommandError> {
    services(&state)?.host_trust(&id, &fingerprint_sha256, mode)
}

#[tauri::command]
pub fn group_list(state: State<'_, AppState>) -> Result<Vec<GroupView>, CommandError> {
    services(&state)?.group_list()
}

#[tauri::command]
pub fn host_reveal_password(
    state: State<'_, AppState>,
    id: String,
) -> Result<Option<String>, CommandError> {
    services(&state)?.host_reveal_password(&id)
}

#[tauri::command]
pub fn group_rename(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<GroupView, CommandError> {
    services(&state)?.group_rename(&id, &name)
}

#[tauri::command]
pub fn group_delete(state: State<'_, AppState>, id: String) -> Result<(), CommandError> {
    services(&state)?.group_delete(&id)
}

#[tauri::command]
pub fn group_create(
    state: State<'_, AppState>,
    name: String,
    sort_order: Option<i64>,
) -> Result<GroupView, CommandError> {
    services(&state)?.group_create(&name, sort_order.unwrap_or(0))
}

#[tauri::command]
pub async fn terminal_connect(
    state: State<'_, AppState>,
    host_id: String,
    cols: u32,
    rows: u32,
    on_event: Channel<TerminalEvent>,
) -> Result<String, CommandError> {
    let flow = Arc::new(crate::terminal_flow::TerminalFlow::default());
    let event_flow = flow.clone();
    let sink: EventSink = Arc::new(move |event| {
        if on_event.send(event).is_err() {
            event_flow.close();
        }
    });
    let lease = services(&state)?;
    lease
        .run(lease.terminal_connect_with_flow(&host_id, cols, rows, sink, flow))
        .await
}

#[tauri::command]
pub fn terminal_ack(
    state: State<'_, AppState>,
    session_id: String,
    bytes: usize,
) -> Result<(), CommandError> {
    state.vault()?.terminal_ack(&session_id, bytes)
}

#[tauri::command]
pub async fn terminal_input(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> Result<(), CommandError> {
    let lease = services(&state)?;
    lease.run(lease.terminal_input(&session_id, data)).await
}

#[tauri::command]
pub async fn terminal_resize(
    state: State<'_, AppState>,
    session_id: String,
    cols: u32,
    rows: u32,
) -> Result<(), CommandError> {
    let lease = services(&state)?;
    lease
        .run(lease.terminal_resize(&session_id, cols, rows))
        .await
}

#[tauri::command]
pub async fn terminal_disconnect(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), CommandError> {
    let lease = services(&state)?;
    lease.run(lease.terminal_disconnect(&session_id)).await
}

#[tauri::command]
pub fn provider_templates() -> Vec<TemplateView> {
    Services::provider_templates()
}

#[tauri::command]
pub fn provider_list(
    state: State<'_, AppState>,
    cursor: Option<String>,
) -> Result<Page<ProviderView>, CommandError> {
    services(&state)?.provider_list(cursor)
}

#[tauri::command]
pub fn provider_save(
    state: State<'_, AppState>,
    input: ProviderInput,
) -> Result<ProviderView, CommandError> {
    services(&state)?.provider_save(input)
}

#[tauri::command]
pub async fn provider_models(
    state: State<'_, AppState>,
    input: ProviderModelsInput,
) -> Result<ModelsView, CommandError> {
    let lease = services(&state)?;
    lease.run(lease.provider_models(input)).await
}

#[tauri::command]
pub fn provider_delete(state: State<'_, AppState>, id: String) -> Result<(), CommandError> {
    services(&state)?.provider_delete(&id)
}

#[tauri::command]
pub async fn agent_start(
    state: State<'_, AppState>,
    host_id: String,
    provider_id: String,
    model_id: Option<String>,
    prompt: String,
    history: Option<Vec<ChatMessageInput>>,
    on_event: Channel<AgentEventView>,
) -> Result<String, CommandError> {
    let sink: Arc<dyn Fn(AgentEventView) + Send + Sync> = Arc::new(move |event| {
        // A closed channel means the window went away; the task keeps running
        // until it finishes or is cancelled, and its audit trail is still written.
        let _ = on_event.send(event);
    });
    let lease = services(&state)?;
    lease
        .run(lease.agent_start_with_model(
            &host_id,
            &provider_id,
            model_id.as_deref(),
            &prompt,
            &history.unwrap_or_default(),
            sink,
        ))
        .await
}

#[tauri::command]
pub async fn chat_send(
    state: State<'_, AppState>,
    provider_id: String,
    model_id: Option<String>,
    messages: Vec<ChatMessageInput>,
    on_event: Channel<ChatEventView>,
) -> Result<String, CommandError> {
    let sink: Arc<dyn Fn(ChatEventView) + Send + Sync> = Arc::new(move |event| {
        let _ = on_event.send(event);
    });
    let lease = services(&state)?;
    lease
        .run(lease.chat_send_with_model(&provider_id, model_id.as_deref(), &messages, sink))
        .await
}

#[tauri::command]
pub fn agent_cancel(state: State<'_, AppState>, task_id: String) -> Result<bool, CommandError> {
    services(&state)?.agent_cancel(&task_id)
}

#[tauri::command]
pub fn chat_session_list(state: State<'_, AppState>) -> Result<Vec<ChatSessionView>, CommandError> {
    services(&state)?.chat_session_list()
}

#[tauri::command]
pub fn chat_session_get(
    state: State<'_, AppState>,
    id: String,
) -> Result<ChatSessionView, CommandError> {
    services(&state)?.chat_session_get(&id)
}

#[tauri::command]
pub fn chat_session_save(
    state: State<'_, AppState>,
    input: ChatSessionInput,
) -> Result<(), CommandError> {
    state.vault()?.snapshot_services()?.chat_session_save(input)
}

#[tauri::command]
pub fn approval_decide(
    state: State<'_, AppState>,
    approval_id: String,
    approve: bool,
    sudo: Option<SudoAnswerInput>,
    approve_root_for_task: bool,
) -> Result<(), CommandError> {
    services(&state)?.approval_decide_with_root_scope(
        &approval_id,
        approve,
        sudo,
        approve_root_for_task,
    )
}

#[tauri::command]
pub fn sudo_options(
    state: State<'_, AppState>,
    approval_id: String,
) -> Result<SudoOptionsView, CommandError> {
    services(&state)?.sudo_options(&approval_id)
}

#[tauri::command]
pub fn host_forget_sudo_password(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), CommandError> {
    services(&state)?.host_forget_sudo_password(&id)
}

#[tauri::command]
pub fn history_list(
    state: State<'_, AppState>,
    host_id: String,
    cursor: Option<String>,
) -> Result<Page<ExecutionView>, CommandError> {
    services(&state)?.history_list(&host_id, cursor)
}

#[tauri::command]
pub fn audit_verify(state: State<'_, AppState>) -> Result<AuditVerifyView, CommandError> {
    services(&state)?.audit_verify()
}

#[tauri::command]
pub async fn provider_test(
    state: State<'_, AppState>,
    id: String,
    kind: String,
) -> Result<ProviderTestView, CommandError> {
    let lease = services(&state)?;
    lease.run(lease.provider_test(&id, &kind)).await
}

#[tauri::command]
pub fn history_query(
    state: State<'_, AppState>,
    host_id: String,
    filter: Option<HistoryFilterInput>,
    cursor: Option<String>,
) -> Result<Page<ExecutionView>, CommandError> {
    services(&state)?.history_query(&host_id, &filter.unwrap_or_default(), cursor)
}

#[tauri::command]
pub fn history_tasks(
    state: State<'_, AppState>,
    host_id: String,
    cursor: Option<String>,
) -> Result<Page<TaskView>, CommandError> {
    services(&state)?.history_tasks(&host_id, cursor)
}

#[tauri::command]
pub fn history_summary(
    state: State<'_, AppState>,
    host_id: String,
) -> Result<HostUsageView, CommandError> {
    services(&state)?.history_summary(&host_id)
}

#[tauri::command]
pub fn task_timeline(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<ExecutionView>, CommandError> {
    services(&state)?.task_timeline(&task_id)
}

#[tauri::command]
pub fn history_export(
    state: State<'_, AppState>,
    host_id: String,
    filter: Option<HistoryFilterInput>,
) -> Result<ExportResult, CommandError> {
    services(&state)?.history_export(&host_id, &filter.unwrap_or_default())
}

#[tauri::command]
pub fn history_purge_output(
    state: State<'_, AppState>,
    host_id: String,
) -> Result<PurgeResult, CommandError> {
    services(&state)?.history_purge_output(&host_id)
}

#[tauri::command]
pub fn preferences_get(state: State<'_, AppState>) -> Result<AiPreferences, CommandError> {
    services(&state)?.preferences_get()
}

#[tauri::command]
pub fn preferences_set(
    state: State<'_, AppState>,
    input: AiPreferencesInput,
) -> Result<AiPreferences, CommandError> {
    services(&state)?.preferences_set(input)
}
