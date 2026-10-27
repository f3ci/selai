//! Per-host AI history through the application service (S-040, S-042): filters,
//! timelines, rename and archive, sanitized export, output purge, preferences.
//! Real OpenSSH host, mock provider.

#![cfg(target_os = "linux")] // Needs OpenSSH sshd and root; see docs/release/milestones.md
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use selai_agent::{COMMAND_TOOL, DIAGNOSTIC_TOOL};
use selai_domain::ErrorCode;
use selai_storage::Database;

mod common;
use common::*;

/// Runs one command task on `host`, denying or approving it. Returns the task id.
async fn command_task(h: &Harness, host: &str, provider: &str, approve: bool) -> String {
    let (on_event, mut rx) = sink();
    let task = h
        .services
        .agent_start(host, provider, "run the command", &[], on_event)
        .await
        .expect("start");
    let waiting = until(&mut rx, |e| {
        matches!(e, TaskEventView::ApprovalRequired { .. })
    })
    .await;
    let approval = match waiting.last() {
        Some(TaskEventView::ApprovalRequired { approval_id, .. }) => approval_id.clone(),
        other => panic!("expected an approval, got {other:?}"),
    };
    h.services
        .approval_decide(&approval, approve, None)
        .expect("decide");
    until(&mut rx, |e| {
        matches!(e, TaskEventView::State { state, .. } if matches!(state.as_str(), "completed" | "failed" | "cancelled"))
    })
    .await;
    task
}

fn all() -> selai_desktop_lib::history_service::HistoryFilterInput {
    Default::default()
}

#[tokio::test]
async fn filters_separate_denied_from_completed_and_timelines_follow_tasks() {
    let h = harness();
    let sshd = Sshd::start();
    let marker = h.dir.path().join("history-marker");
    let mock = model(vec![
        Turn::Tool(
            COMMAND_TOOL,
            json!({"command": format!("touch {}", marker.display())}),
        ),
        Turn::Text("not run"),
        Turn::Tool(COMMAND_TOOL, json!({"command": "echo approved-run"})),
        Turn::Text("ran"),
    ])
    .await;
    let (host, provider) = ready(&h, &sshd, &mock).await;

    let denied_task = command_task(&h, &host, &provider, false).await;
    let approved_task = command_task(&h, &host, &provider, true).await;

    let mut filter = all();
    filter.states = vec!["denied".into()];
    let denied = h
        .services
        .history_query(&host, &filter, None)
        .expect("denied");
    assert_eq!(denied.items.len(), 1);
    assert_eq!(denied.items[0].task_id, denied_task);

    let mut filter = all();
    filter.states = vec!["completed".into()];
    let completed = h
        .services
        .history_query(&host, &filter, None)
        .expect("completed");
    assert_eq!(completed.items.len(), 1);
    assert_eq!(completed.items[0].task_id, approved_task);
    assert!(!marker.exists(), "the denied command must not run");

    assert_eq!(
        h.services
            .history_query(&host, &all(), None)
            .expect("all")
            .items
            .len(),
        2
    );
    assert_eq!(
        h.services
            .task_timeline(&denied_task)
            .expect("timeline")
            .len(),
        1
    );
    let tasks = h.services.history_tasks(&host, None).expect("tasks");
    assert_eq!(tasks.items.len(), 2);
    assert!(h.services.audit_verify().expect("verify").ok);
}

#[tokio::test]
async fn invalid_filters_are_refused_with_a_reason() {
    let h = harness();
    let sshd = Sshd::start();
    let mock = model(vec![]).await;
    let (host, _provider) = ready(&h, &sshd, &mock).await;

    let mut bad_state = all();
    bad_state.states = vec!["bogus".into()];
    assert_eq!(
        h.services
            .history_query(&host, &bad_state, None)
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );

    let mut backwards = all();
    backwards.since_ms = Some(20);
    backwards.until_ms = Some(10);
    assert_eq!(
        h.services
            .history_query(&host, &backwards, None)
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn stored_output_is_redacted_and_the_export_never_contains_the_secret() {
    let h = harness();
    let sshd = Sshd::start();
    let mock = model(vec![
        Turn::Tool(
            COMMAND_TOOL,
            json!({"command": "echo password=SuperSecret123"}),
        ),
        Turn::Text("printed"),
    ])
    .await;
    let (host, provider) = ready(&h, &sshd, &mock).await;
    command_task(&h, &host, &provider, true).await;

    let rows = h
        .services
        .history_query(&host, &all(), None)
        .expect("history");
    let preview = rows.items[0].stdout_preview.clone().unwrap_or_default();
    assert!(
        preview.contains("password"),
        "the output must have been captured: {preview}"
    );
    assert!(
        !preview.contains("SuperSecret123"),
        "stored preview must be redacted: {preview}"
    );

    let export = h.services.history_export(&host, &all()).expect("export");
    assert_eq!(export.executions, 1);
    assert!(!export.truncated_rows);
    let text = std::fs::read_to_string(&export.path).expect("read export");
    assert!(
        !text.contains("SuperSecret123"),
        "the export must not contain the secret"
    );
    let document: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(document["format"], "selai-history-v1");
    assert_eq!(document["rowCount"], 1);
    assert!(document["disclosure"]
        .as_str()
        .unwrap_or_default()
        .contains("cannot find every"));

    // The export itself is audited, by row count only.
    let db = Database::open(&h.db_path).expect("reopen for audit read");
    assert!(db.verify_audit_chain().expect("verify").is_ok());
    let exports: i64 = db
        .connection_for_provider_test()
        .query_row(
            "SELECT COUNT(*) FROM audit_events WHERE event_type = 'history_exported'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(exports, 1);
}

#[tokio::test]
async fn purge_clears_output_and_keeps_the_command_record() {
    let h = harness();
    let sshd = Sshd::start();
    let mock = model(vec![
        Turn::Tool(DIAGNOSTIC_TOOL, json!({"operation": "disk_usage"})),
        Turn::Text("checked"),
    ])
    .await;
    let (host, provider) = ready(&h, &sshd, &mock).await;
    let (on_event, mut rx) = sink();
    h.services
        .agent_start(&host, &provider, "check disk usage", &[], on_event)
        .await
        .expect("start");
    until(&mut rx, is_terminal).await;

    let before = h
        .services
        .history_query(&host, &all(), None)
        .expect("before");
    assert!(before.items[0].stdout_preview.is_some());

    let purged = h.services.history_purge_output(&host).expect("purge");
    assert_eq!(purged.executions, 1);

    let after = h
        .services
        .history_query(&host, &all(), None)
        .expect("after");
    assert_eq!(after.items[0].stdout_preview, None);
    assert_eq!(after.items[0].command_redacted, "df -hP");
    assert_eq!(after.items[0].state, "completed");
    assert!(h.services.audit_verify().expect("verify").ok);
}

#[tokio::test]
async fn history_survives_rename_and_archive_and_exports_as_archived() {
    let h = harness();
    let sshd = Sshd::start();
    let mock = model(vec![
        Turn::Tool(DIAGNOSTIC_TOOL, json!({"operation": "uptime"})),
        Turn::Text("up"),
    ])
    .await;
    let (host, provider) = ready(&h, &sshd, &mock).await;
    let (on_event, mut rx) = sink();
    h.services
        .agent_start(&host, &provider, "how long up", &[], on_event)
        .await
        .expect("start");
    until(&mut rx, is_terminal).await;

    let current = h.services.host_get(&host).expect("get");
    let mut renamed = password_host("renamed-web", sshd.port);
    renamed.id = Some(host.clone());
    renamed.expected_updated_at_ms = Some(current.updated_at_ms);
    h.services.host_save(renamed).expect("rename");
    let after_rename = h
        .services
        .history_query(&host, &all(), None)
        .expect("after rename");
    assert_eq!(
        after_rename.items.len(),
        1,
        "history stays with the host id"
    );

    h.services.host_archive(&host).expect("archive");
    let after_archive = h
        .services
        .history_query(&host, &all(), None)
        .expect("after archive");
    assert_eq!(
        after_archive.items.len(),
        1,
        "archived history stays readable"
    );

    let export = h
        .services
        .history_export(&host, &all())
        .expect("export archived");
    let document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&export.path).expect("read")).expect("json");
    assert_eq!(document["host"]["archived"], true);
    assert_eq!(document["host"]["label"], "renamed-web");
}

#[tokio::test]
async fn preferences_persist_and_refuse_bad_values() {
    let h = harness();
    let sshd = Sshd::start();
    let mock = model(vec![]).await;
    let (_host, provider) = ready(&h, &sshd, &mock).await;

    let defaults = h.services.preferences_get().expect("defaults");
    assert_eq!(defaults.output_retention_days, 30);
    assert_eq!(defaults.last_provider_id, None);

    let refused = h
        .services
        .preferences_set(selai_desktop_lib::history_service::AiPreferencesInput {
            last_provider_id: None,
            output_retention_days: Some(0),
            pinned_host_id: None,
            task_max_steps: None,
            task_max_minutes: None,
        })
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);

    let refused_task_limit = h
        .services
        .preferences_set(selai_desktop_lib::history_service::AiPreferencesInput {
            last_provider_id: None,
            output_retention_days: None,
            pinned_host_id: None,
            task_max_steps: Some(301),
            task_max_minutes: None,
        })
        .unwrap_err();
    assert_eq!(refused_task_limit.code, ErrorCode::InvalidInput);

    let unknown = h
        .services
        .preferences_set(selai_desktop_lib::history_service::AiPreferencesInput {
            last_provider_id: Some("00000000-0000-4000-8000-0000000000aa".into()),
            output_retention_days: None,
            pinned_host_id: None,
            task_max_steps: None,
            task_max_minutes: None,
        })
        .unwrap_err();
    assert_eq!(unknown.code, ErrorCode::NotFound);

    h.services
        .preferences_set(selai_desktop_lib::history_service::AiPreferencesInput {
            last_provider_id: Some(provider.clone()),
            output_retention_days: Some(14),
            pinned_host_id: None,
            task_max_steps: Some(24),
            task_max_minutes: Some(25),
        })
        .expect("set");

    let reopened =
        selai_desktop_lib::service::Services::open(&h.db_path, h.store.clone()).expect("reopen");
    let saved = reopened.preferences_get().expect("saved");
    assert_eq!(saved.last_provider_id, Some(provider));
    assert_eq!(saved.output_retention_days, 14);
    assert_eq!(saved.task_max_steps, 24);
    assert_eq!(saved.task_max_minutes, 25);
}
