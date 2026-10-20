//! Synthetic data only: never opens the user's app directory or OS keyring.
#![allow(clippy::unwrap_used)]
use super::*;
use crate::dto::HostInput;
use selai_secrets::{MemoryStore, SshSecretKind};
const PHRASE: &str = "forest river orange telescope";
fn phrase() -> Zeroizing<String> {
    Zeroizing::new(PHRASE.into())
}

#[test]
fn idle_compaction_reclaims_space_and_retains_encrypted_data() {
    let dir = tempfile::tempdir().unwrap();
    let manager = VaultManager::new(dir.path().into()).unwrap();
    manager.initialize(phrase(), &MemoryStore::new()).unwrap();
    let lease = manager.services().unwrap();
    let sentinel = "retained-sensitive-setting";
    {
        let mut db = lease.db.lock().unwrap();
        db.set_setting(
            "maintenance",
            &serde_json::json!("x".repeat(2 * 1024 * 1024)),
            1,
        )
        .unwrap();
        db.set_setting("maintenance", &serde_json::json!(sentinel), 2)
            .unwrap();
    }
    assert!(
        manager.compact().is_err(),
        "an existing lease must block maintenance"
    );
    let before = lease.db.lock().unwrap().storage_usage().unwrap().0;
    drop(lease);
    let result: serde_json::Value = serde_json::from_str(&manager.compact().unwrap()).unwrap();
    assert!(result["databaseBytes"].as_u64().unwrap() < before);
    let lease = manager.services().unwrap();
    assert_eq!(
        lease.db.lock().unwrap().get_setting("maintenance").unwrap(),
        Some(serde_json::json!(sentinel))
    );
    let (path, key) = {
        let inner = manager.inner.lock().unwrap();
        let vault = inner.vault.as_ref().unwrap();
        (vault.database_path(), *vault.key())
    };
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes
        .windows(sentinel.len())
        .any(|part| part == sentinel.as_bytes()));
    let reopened = Database::open_encrypted(&path, &key).unwrap();
    reopened.integrity_check().unwrap();
    assert_eq!(
        reopened.get_setting("maintenance").unwrap(),
        Some(serde_json::json!(sentinel))
    );
}

#[tokio::test]
async fn status_notifications_are_versioned_and_lock_is_published_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let manager = VaultManager::new(dir.path().into()).unwrap();
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let sink = statuses.clone();
    manager
        .watch_status(Arc::new(move |status| sink.lock().unwrap().push(status)))
        .unwrap();
    manager.publish_status();
    manager.publish_status();
    assert_eq!(statuses.lock().unwrap().len(), 1);
    manager.initialize(phrase(), &MemoryStore::new()).unwrap();
    manager.publish_status();
    manager.clone().lock(true).await.unwrap();
    manager.publish_status();
    let statuses = statuses.lock().unwrap();
    assert!(statuses.iter().any(|status| status.phase == "locking"));
    assert_eq!(statuses.last().unwrap().phase, "locked");
    assert!(statuses
        .windows(2)
        .all(|pair| pair[0].revision < pair[1].revision));
}

#[test]
fn generated_key_algorithms_roundtrip_in_the_vault_and_can_be_used_by_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let manager = VaultManager::new(dir.path().into()).unwrap();
    manager.initialize(phrase(), &MemoryStore::new()).unwrap();
    let lease = manager.services().unwrap();
    for (algorithm, prefix) in [
        ("ed25519", "ssh-ed25519"),
        ("rsa-2048", "ssh-rsa"),
        ("rsa-3072", "ssh-rsa"),
        ("rsa-4096", "ssh-rsa"),
        ("rsa-8192", "ssh-rsa"),
        ("ecdsa-p256", "ecdsa-sha2-nistp256"),
        ("ecdsa-p384", "ecdsa-sha2-nistp384"),
        ("ecdsa-p521", "ecdsa-sha2-nistp521"),
    ] {
        let key = lease
            .ssh_key_generate(algorithm.into(), algorithm.into())
            .unwrap();
        assert_eq!(key.algorithm, prefix);
        assert!(key.public_key.starts_with(&format!("{prefix} ")));
        assert!(key.fingerprint_sha256.starts_with("SHA256:"));
        let private_bytes = lease
            .secrets
            .get(&SecretKey::managed_ssh_key(
                key.id.parse().unwrap(),
                SshSecretKind::ManagedPrivateKey,
            ))
            .unwrap()
            .unwrap();
        let private = russh::keys::decode_secret_key(
            std::str::from_utf8(private_bytes.expose_for_transport()).unwrap(),
            None,
        )
        .unwrap();
        selai_ssh::validate_private_key(&private).unwrap();
        assert_eq!(private.public_key().to_openssh().unwrap(), key.public_key);
        if let Some(rsa) = private.key_data().rsa() {
            assert_eq!(
                rsa.key_size().to_string(),
                algorithm.strip_prefix("rsa-").unwrap()
            );
        }
        let mut profile = host();
        profile.label = algorithm.into();
        profile.auth_method = "private_key".into();
        profile.password = None;
        profile.private_key_path = Some(format!("vault://key:{}", key.id));
        assert!(lease.host_save(profile).unwrap().credential_configured);
    }
    assert_eq!(lease.ssh_key_list().unwrap().len(), 8);
    assert!(!serde_json::to_string(&lease.ssh_key_list().unwrap())
        .unwrap()
        .contains("PRIVATE KEY"));
    for unsupported in ["rsa-1024", "dsa", "invalid"] {
        assert_eq!(
            lease
                .ssh_key_generate("Unsupported".into(), unsupported.into())
                .unwrap_err()
                .code,
            selai_domain::ErrorCode::InvalidInput
        );
    }
    assert_eq!(lease.ssh_key_list().unwrap().len(), 8);
}
fn host() -> HostInput {
    HostInput {
        id: None,
        expected_updated_at_ms: None,
        label: "synthetic-server".into(),
        group_id: None,
        proxy_id: None,
        hostname: "example.test".into(),
        port: 22,
        username: "test".into(),
        auth_method: "password".into(),
        private_key_path: None,
        connect_timeout_seconds: 10,
        keep_alive_seconds: 30,
        terminal_type: "xterm-256color".into(),
        notes: Some("private notes marker".into()),
        password: Some("synthetic-password-marker".into()),
        key_passphrase: None,
    }
}
async fn finish_lock(manager: &Arc<VaultManager>, force: bool) {
    let job = tokio::spawn(manager.clone().lock(force));
    loop {
        let status = manager.status().unwrap();
        if status.flush_required {
            manager.flush_complete().unwrap();
        }
        if status.phase == "locked" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    job.await.unwrap().unwrap();
}

#[tokio::test]
async fn migrates_all_data_then_closes_database_and_denies_locked_access() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = Arc::new(MemoryStore::new());
    let old = Services::open(&dir.path().join("selai.sqlite3"), legacy.clone()).unwrap();
    let host = old.host_save(host()).unwrap();
    old.db
        .lock()
        .unwrap()
        .set_setting("test-chat", &serde_json::json!("private chat marker"), 1)
        .unwrap();
    std::fs::create_dir(dir.path().join("exports")).unwrap();
    std::fs::write(
        dir.path().join("exports/old.json"),
        b"private export marker",
    )
    .unwrap();
    drop(old);
    let manager = VaultManager::new(dir.path().into()).unwrap();
    assert!(
        VaultManager::new(dir.path().into()).is_err(),
        "reject second process owner"
    );
    assert!(manager.services().is_err());
    let code = manager.initialize(phrase(), legacy.as_ref()).unwrap();
    assert!(!code.is_empty());
    assert!(!dir.path().join("selai.sqlite3").exists());
    assert!(!dir.path().join("exports/old.json").exists());
    assert!(legacy.is_empty());
    let lease = manager.services().unwrap();
    let retained_db = lease.db.clone();
    assert_eq!(
        lease.host_reveal_password(&host.id).unwrap().as_deref(),
        Some("synthetic-password-marker")
    );
    assert_eq!(lease.host_get(&host.id).unwrap().label, "synthetic-server");
    let id = host.id.parse().unwrap();
    assert_eq!(
        lease
            .secrets
            .get(&SecretKey::ssh(id, SshSecretKind::Password))
            .unwrap()
            .unwrap()
            .expose_for_transport(),
        b"synthetic-password-marker"
    );
    let doc = lease
        .history_export(
            &host.id,
            &crate::history_service::HistoryFilterInput::default(),
        )
        .unwrap();
    let bytes = std::fs::read(&doc.path).unwrap();
    assert!(doc.path.ends_with(".selai-history"));
    assert!(!String::from_utf8_lossy(&bytes).contains("synthetic-server"));
    assert!(Vault::open_document(&bytes, PHRASE)
        .unwrap()
        .windows(b"synthetic-server".len())
        .any(|v| v == b"synthetic-server"));
    drop(lease);
    finish_lock(&manager, false).await;
    assert!(manager.services().is_err());
    assert!(manager.snapshot_services().is_err());
    assert!(
        retained_db
            .lock()
            .unwrap()
            .vault_secret("anything")
            .is_err(),
        "retained Arc has no decrypted DB"
    );
    assert!(manager
        .unlock(Zeroizing::new("incorrect phrase".into()))
        .is_err());
    manager.inner.lock().unwrap().retry_at = Instant::now();
    manager.unlock(phrase()).unwrap();
    assert_eq!(
        manager
            .services()
            .unwrap()
            .db
            .lock()
            .unwrap()
            .get_setting("test-chat")
            .unwrap(),
        Some(serde_json::json!("private chat marker"))
    );
    finish_lock(&manager, true).await;
}

#[tokio::test]
async fn lock_waits_for_inflight_operations_blocks_admission_and_accepts_final_chat_save() {
    let dir = tempfile::tempdir().unwrap();
    let manager = VaultManager::new(dir.path().into()).unwrap();
    manager.initialize(phrase(), &MemoryStore::new()).unwrap();
    let lease = manager.services().unwrap();
    let job = tokio::spawn(manager.clone().lock(true));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(manager.status().unwrap().phase, "locking");
    assert!(manager.services().is_err());
    assert!(!job.is_finished());
    let snapshot = manager.snapshot_services().unwrap();
    snapshot
        .db
        .lock()
        .unwrap()
        .set_setting("final-snapshot", &serde_json::json!("saved"), 1)
        .unwrap();
    drop(snapshot);
    drop(lease);
    loop {
        if manager.status().unwrap().flush_required {
            manager.flush_complete().unwrap();
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    job.await.unwrap().unwrap();
    assert_eq!(manager.status().unwrap().phase, "locked");
}

#[tokio::test]
async fn lock_cancels_network_rpcs_instead_of_waiting_for_their_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let manager = VaultManager::new(dir.path().into()).unwrap();
    manager.initialize(phrase(), &MemoryStore::new()).unwrap();
    let lease = manager.services().unwrap();
    let operation = tokio::spawn(async move {
        lease
            .run(std::future::pending::<Result<(), CommandError>>())
            .await
    });
    finish_lock(&manager, true).await;
    assert!(operation.await.unwrap().is_err());
    assert_eq!(manager.status().unwrap().phase, "locked");
}

#[cfg(unix)]
#[tokio::test]
async fn lock_preserves_a_live_terminal_and_reuses_it_after_unlock() {
    let dir = tempfile::tempdir().unwrap();
    let manager = VaultManager::new(dir.path().into()).unwrap();
    manager.initialize(phrase(), &MemoryStore::new()).unwrap();
    let lease = manager.services().unwrap();
    let host = lease.local_host().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = lease
        .terminal_connect(
            &host.id,
            80,
            24,
            Arc::new(move |event| {
                let _ = tx.send(event);
            }),
        )
        .await
        .unwrap();
    assert_eq!(lease.terminal_count(), 1);
    drop(lease);
    tokio::time::timeout(Duration::from_secs(10), finish_lock(&manager, true))
        .await
        .unwrap();
    assert_eq!(manager.status().unwrap().terminals, 1);
    assert_eq!(manager.status().unwrap().phase, "locked");
    assert!(manager.services().is_err());
    manager.terminal_ack(&session, 0).unwrap();
    manager.unlock(phrase()).unwrap();
    let lease = manager.services().unwrap();
    assert_eq!(lease.terminal_count(), 1);
    assert_eq!(lease.terminal_host(&session).unwrap().to_string(), host.id);
    lease
        .terminal_input(&session, "printf 'standby-%s\\n' 42\n".into())
        .await
        .unwrap();
    let mut output = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        use base64::Engine as _;
        while let Some(event) = rx.recv().await {
            match event {
                crate::dto::TerminalEvent::Data { data } => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .unwrap();
                    output.push_str(&String::from_utf8_lossy(&bytes));
                    if output.contains("standby-42") {
                        break;
                    }
                }
                crate::dto::TerminalEvent::Closed { .. } => {
                    panic!("terminal closed during standby")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(output.contains("standby-42"));
    lease.terminal_disconnect(&session).await.unwrap();
}

#[test]
fn migration_failure_preserves_legacy_database_and_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = Arc::new(MemoryStore::new());
    let old = Services::open(&dir.path().join("selai.sqlite3"), legacy.clone()).unwrap();
    old.host_save(host()).unwrap();
    drop(old);
    legacy.fail_all_with(SecretError::Locked);
    let manager = VaultManager::new(dir.path().into()).unwrap();
    assert!(manager.initialize(phrase(), legacy.as_ref()).is_err());
    assert!(dir.path().join("selai.sqlite3").exists());
    assert!(!dir.path().join(selai_vault::MANIFEST).exists());
    assert_eq!(legacy.len(), 1);
    assert_eq!(manager.status().unwrap().phase, "setup");
}

#[test]
fn idle_is_based_on_user_activity_and_timeout_is_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let manager = VaultManager::new(dir.path().into()).unwrap();
    manager.initialize(phrase(), &MemoryStore::new()).unwrap();
    manager.timeout(5).unwrap();
    manager.inner.lock().unwrap().activity = Instant::now() - Duration::from_secs(301);
    assert!(manager.idle());
    assert!(manager.services().is_err());
    assert!(
        manager.activity().is_err(),
        "late activity cannot revive expired access"
    );
}

#[test]
fn pending_credential_cleanup_survives_forgetting_and_changed_source_files_are_preserved() {
    struct RefusesDelete(MemoryStore);
    impl SecretStore for RefusesDelete {
        fn get(&self, key: &SecretKey) -> Result<Option<SecretValue>, SecretError> {
            self.0.get(key)
        }
        fn put(&self, key: &SecretKey, value: &SecretValue) -> Result<(), SecretError> {
            self.0.put(key, value)
        }
        fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
            Err(SecretError::Locked)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let legacy = Arc::new(RefusesDelete(MemoryStore::new()));
    let old = Services::open(&dir.path().join("selai.sqlite3"), legacy.clone()).unwrap();
    let saved = old.host_save(host()).unwrap();
    drop(old);
    let manager = VaultManager::new(dir.path().into()).unwrap();
    manager.initialize(phrase(), legacy.as_ref()).unwrap();
    assert!(manager.status().unwrap().cleanup_pending);
    let lease = manager.services().unwrap();
    let key = SecretKey::ssh(saved.id.parse().unwrap(), SshSecretKind::Password);
    lease.secrets.delete(&key).unwrap();
    assert!(lease.secrets.get(&key).unwrap().is_none());
    assert_eq!(
        lease
            .db
            .lock()
            .unwrap()
            .pending_legacy_credentials()
            .unwrap(),
        vec![key.account().to_owned()]
    );
    std::fs::write(dir.path().join("selai.sqlite3"), b"changed after migration").unwrap();
    assert!(!cleanup(dir.path(), &lease, &legacy.0));
    assert!(legacy.0.is_empty());
    assert_eq!(
        std::fs::read(dir.path().join("selai.sqlite3")).unwrap(),
        b"changed after migration"
    );
    assert!(lease
        .db
        .lock()
        .unwrap()
        .pending_legacy_credentials()
        .unwrap()
        .is_empty());
}

#[test]
fn private_key_bytes_are_managed_and_backup_restores_to_fresh_directory() {
    let dir = tempfile::tempdir().unwrap();
    let manager = VaultManager::new(dir.path().into()).unwrap();
    manager.initialize(phrase(), &MemoryStore::new()).unwrap();
    let source = tempfile::tempdir().unwrap();
    let key_path = source.path().join("id_ed25519");
    std::fs::write(&key_path, b"synthetic private key bytes").unwrap();
    let lease = manager.services().unwrap();
    let mut input = host();
    input.auth_method = "private_key".into();
    input.password = None;
    input.private_key_path = Some(key_path.display().to_string());
    let saved = lease.host_save(input).unwrap();
    assert_eq!(
        saved.private_key_path,
        Some(format!("vault://{}", saved.id))
    );
    assert!(key_path.exists());
    assert_eq!(
        lease
            .secrets
            .get(&SecretKey::ssh(
                saved.id.parse().unwrap(),
                SshSecretKind::PrivateKey
            ))
            .unwrap()
            .unwrap()
            .expose_for_transport(),
        b"synthetic private key bytes"
    );
    drop(lease);
    let backup = source.path().join("portable.selai");
    manager.backup(phrase(), &backup).unwrap();
    assert!(
        manager.restore(phrase(), &backup).is_err(),
        "never overwrite an existing vault"
    );
    let destination = tempfile::tempdir().unwrap();
    let restored = VaultManager::new(destination.path().into()).unwrap();
    restored.restore(phrase(), &backup).unwrap();
    assert_eq!(
        restored
            .services()
            .unwrap()
            .host_get(&saved.id)
            .unwrap()
            .private_key_path,
        saved.private_key_path
    );
}
