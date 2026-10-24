//! Invariant tests for the storage layer. Each one corresponds to a rule in
//! AGENTS.md, SELAI_V1_ENGINEERING_SPEC.md or the execution backlog.

use rusqlite::{params, Connection};

use crate::{migrate, Database, Migration, StorageError};

const T0: i64 = 1_000;

fn db() -> Database {
    Database::open_in_memory().expect("open in-memory database")
}

fn insert_host(db: &Database, id: &str, label: &str) -> rusqlite::Result<usize> {
    db.conn().execute(
        "INSERT INTO hosts(id, label, hostname, username, auth_method, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, 'host.example', 'ops', 'password', ?3, ?3)",
        params![id, label, T0],
    )
}

fn insert_task(db: &Database, id: &str, host: &str, state: &str) -> rusqlite::Result<usize> {
    db.conn().execute(
        "INSERT INTO agent_tasks(id, target_host_id, model_id_snapshot, state, created_at_ms)
         VALUES (?1, ?2, 'model-x', ?3, ?4)",
        params![id, host, state, T0],
    )
}

fn insert_exec(
    db: &Database,
    id: &str,
    task: &str,
    host: &str,
    sha: &str,
    state: &str,
) -> rusqlite::Result<usize> {
    db.conn().execute(
        "INSERT INTO command_executions(id, task_id, host_id, model_id_snapshot, tool_name,
           command_redacted, command_sha256, state, created_at_ms)
         VALUES (?1, ?2, ?3, 'model-x', 'ssh.run_command', 'df -h', ?4, ?5, ?6)",
        params![id, task, host, sha, state, T0],
    )
}

fn set_task_state(db: &Database, id: &str, state: &str) -> rusqlite::Result<usize> {
    db.conn().execute(
        "UPDATE agent_tasks SET state = ?2 WHERE id = ?1",
        params![id, state],
    )
}

fn set_exec_state(db: &Database, id: &str, state: &str) -> rusqlite::Result<usize> {
    db.conn().execute(
        "UPDATE command_executions SET state = ?2 WHERE id = ?1",
        params![id, state],
    )
}

fn insert_approval(
    db: &Database,
    id: &str,
    task: &str,
    exec: &str,
    host: &str,
    sha: &str,
) -> rusqlite::Result<usize> {
    db.conn().execute(
        "INSERT INTO approval_requests(id, task_id, execution_id, host_id, command_sha256,
           policy_version, state, created_at_ms, expires_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, 'policy-1', 'pending', ?6, ?7)",
        params![id, task, exec, host, sha, T0, T0 + 300_000],
    )
}

fn set_approval_state(db: &Database, id: &str, state: &str) -> rusqlite::Result<usize> {
    db.conn().execute(
        "UPDATE approval_requests SET state = ?2 WHERE id = ?1",
        params![id, state],
    )
}

fn scalar(db: &Database, sql: &str) -> String {
    let value: rusqlite::types::Value = db
        .conn()
        .query_row(sql, [], |row| row.get(0))
        .expect("scalar query");
    match value {
        rusqlite::types::Value::Text(s) => s,
        rusqlite::types::Value::Integer(i) => i.to_string(),
        other => format!("{other:?}"),
    }
}

// ---- schema and connection settings ---------------------------------------

#[test]
fn fresh_database_is_at_latest_version() {
    let db = db();
    assert_eq!(db.schema_version().expect("version"), 11);
    let tables: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(
        tables, 19,
        "16 app tables, 2 vault tables, and schema_migrations"
    );
}

#[test]
fn foreign_keys_are_enforced_on_every_connection() {
    let db = db();
    let err = db
        .conn()
        .execute(
            "INSERT INTO hosts(id, label, hostname, username, auth_method, group_id,
               created_at_ms, updated_at_ms)
             VALUES ('h1', 'web', 'x', 'u', 'password', 'missing-group', 1, 1)",
            [],
        )
        .expect_err("dangling group_id must fail");
    assert!(matches!(
        StorageError::from(err),
        StorageError::Constraint(_)
    ));
}

#[test]
fn file_database_uses_wal_and_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("selai.sqlite3");
    {
        let db = Database::open(&path).expect("first open");
        assert_eq!(scalar(&db, "PRAGMA journal_mode"), "wal");
        insert_host(&db, "00000000-0000-4000-8000-000000000001", "web").expect("insert");
    }
    let db = Database::open(&path).expect("reopen is a no-op migration");
    assert_eq!(db.schema_version().expect("version"), 11);
    assert_eq!(scalar(&db, "PRAGMA foreign_keys"), "1");
    assert_eq!(
        scalar(&db, "SELECT label FROM hosts WHERE kind = 'ssh'"),
        "web"
    );
}

#[test]
fn busy_database_maps_to_storage_busy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("busy.sqlite3");
    let holder = Database::open(&path).expect("open");
    holder
        .conn()
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold write lock");

    let other = Connection::open(&path).expect("second connection");
    other
        .busy_timeout(std::time::Duration::from_millis(0))
        .expect("timeout");
    let err = other
        .execute_batch("BEGIN IMMEDIATE")
        .expect_err("must be busy");
    assert!(matches!(StorageError::from(err), StorageError::Busy));

    holder.conn().execute_batch("ROLLBACK").expect("release");
}

#[test]
fn failed_migration_rolls_back_completely() {
    let mut conn = Connection::open_in_memory().expect("conn");
    let migrations = [
        Migration {
            version: 1,
            sql: "CREATE TABLE a (x INTEGER);",
        },
        Migration {
            version: 2,
            sql: "CREATE TABLE b (x INTEGER); THIS IS NOT SQL;",
        },
    ];
    let err = migrate::apply(&mut conn, &migrations, T0).expect_err("bad sql");
    assert!(matches!(err, StorageError::Migration { version: 2, .. }));

    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .expect("version");
    assert_eq!(version, 1, "version 2 must not be recorded");
    let b_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'b'",
            [],
            |r| r.get(0),
        )
        .expect("probe");
    assert_eq!(
        b_exists, 0,
        "partial DDL from failed migration must be rolled back"
    );
}

#[test]
fn newer_schema_is_refused_not_downgraded() {
    let mut conn = Connection::open_in_memory().expect("conn");
    migrate::apply(&mut conn, migrate::MIGRATIONS, T0).expect("migrate");
    conn.execute(
        "INSERT INTO schema_migrations(version, applied_at_ms) VALUES (99, 1)",
        [],
    )
    .expect("simulate future schema");
    let err = migrate::apply(&mut conn, migrate::MIGRATIONS, T0).expect_err("too new");
    assert!(matches!(
        err,
        StorageError::SchemaTooNew {
            found: 99,
            supported: 11
        }
    ));
}

#[test]
fn migration_versions_must_increase() {
    let mut conn = Connection::open_in_memory().expect("conn");
    let bad = [
        Migration {
            version: 2,
            sql: "",
        },
        Migration {
            version: 2,
            sql: "",
        },
    ];
    assert!(migrate::apply(&mut conn, &bad, T0).is_err());
}

// ---- host identity and labels ----------------------------------------------

#[test]
fn active_labels_are_unique_case_insensitively() {
    let db = db();
    insert_host(
        &db,
        "00000000-0000-4000-8000-000000000001",
        "production-web",
    )
    .expect("first");
    let dup = insert_host(
        &db,
        "00000000-0000-4000-8000-000000000002",
        "PRODUCTION-WEB",
    );
    assert!(dup.is_err(), "case-variant duplicate must be rejected");
}

#[test]
fn archived_label_can_be_reused() {
    let db = db();
    let a = "00000000-0000-4000-8000-000000000001";
    insert_host(&db, a, "production-web").expect("first");
    db.conn()
        .execute(
            "UPDATE hosts SET archived_at_ms = 2 WHERE id = ?1",
            params![a],
        )
        .expect("archive");
    insert_host(
        &db,
        "00000000-0000-4000-8000-000000000002",
        "Production-Web",
    )
    .expect("label free after archive");
}

#[test]
fn host_id_is_immutable() {
    let db = db();
    let id = "00000000-0000-4000-8000-000000000001";
    insert_host(&db, id, "web").expect("insert");
    let err = db
        .conn()
        .execute("UPDATE hosts SET id = 'other' WHERE id = ?1", params![id])
        .expect_err("id change must fail");
    assert!(err.to_string().contains("immutable"));
    db.conn()
        .execute(
            "UPDATE hosts SET label = 'web-renamed' WHERE id = ?1",
            params![id],
        )
        .expect("label change is allowed");
}

// ---- single-host scope and binding -----------------------------------------

const H1: &str = "00000000-0000-4000-8000-000000000001";
const H2: &str = "00000000-0000-4000-8000-000000000002";
const T1: &str = "00000000-0000-4000-8000-0000000000a1";
const E1: &str = "00000000-0000-4000-8000-0000000000e1";
const E2: &str = "00000000-0000-4000-8000-0000000000e2";
const A1: &str = "00000000-0000-4000-8000-0000000000b1";
const SHA: &str = "aaaaaaaa";

fn scoped_db() -> Database {
    let db = db();
    insert_host(&db, H1, "production-web").expect("h1");
    insert_host(&db, H2, "staging").expect("h2");
    insert_task(&db, T1, H1, "created").expect("task");
    db
}

#[test]
fn execution_on_a_different_host_is_rejected() {
    let db = scoped_db();
    let err =
        insert_exec(&db, E1, T1, H2, SHA, "proposed").expect_err("cross-host execution must fail");
    assert!(err.to_string().contains("target host"));
    insert_exec(&db, E1, T1, H1, SHA, "proposed").expect("same host is fine");
}

#[test]
fn execution_cannot_be_moved_to_another_host() {
    let db = scoped_db();
    insert_exec(&db, E1, T1, H1, SHA, "proposed").expect("insert");
    assert!(db
        .conn()
        .execute("UPDATE command_executions SET host_id = ?1", params![H2])
        .is_err());
}

#[test]
fn task_target_host_is_immutable() {
    let db = scoped_db();
    let err = db
        .conn()
        .execute("UPDATE agent_tasks SET target_host_id = ?1", params![H2])
        .expect_err("retargeting must fail");
    assert!(err.to_string().contains("immutable"));
}

#[test]
fn approval_must_bind_exact_execution_digest_and_host() {
    let db = scoped_db();
    insert_task(&db, "00000000-0000-4000-8000-0000000000a2", H1, "created").expect("task2");
    insert_exec(&db, E1, T1, H1, SHA, "proposed").expect("exec");

    assert!(insert_approval(&db, A1, T1, E1, H1, "tampered-digest").is_err());
    assert!(insert_approval(&db, A1, T1, E1, H2, SHA).is_err());
    assert!(
        insert_approval(&db, A1, T1, E2, H1, SHA).is_err(),
        "unknown execution"
    );
    insert_approval(&db, A1, T1, E1, H1, SHA).expect("exact binding");
}

#[test]
fn attempt_identity_is_immutable_but_state_is_not() {
    let db = scoped_db();
    insert_exec(&db, E1, T1, H1, SHA, "proposed").expect("exec");
    assert!(db
        .conn()
        .execute(
            "UPDATE command_executions SET command_redacted = 'rm -rf /'",
            []
        )
        .is_err());
    assert!(db
        .conn()
        .execute("UPDATE command_executions SET command_sha256 = 'other'", [])
        .is_err());
    set_exec_state(&db, E1, "running").expect("state change is allowed");
}

// ---- state machines --------------------------------------------------------

#[test]
fn task_state_machine_matches_domain_rules() {
    let db = scoped_db();
    assert!(
        set_task_state(&db, T1, "running").is_err(),
        "created -> running skips steps"
    );
    for next in [
        "validating_scope",
        "ready",
        "running",
        "awaiting_approval",
        "running",
        "completed",
    ] {
        set_task_state(&db, T1, next).unwrap_or_else(|e| panic!("-> {next}: {e}"));
    }
    assert!(
        set_task_state(&db, T1, "running").is_err(),
        "completed is final"
    );
    assert!(
        set_task_state(&db, T1, "interrupted").is_err(),
        "completed is final"
    );
}

#[test]
fn execution_state_machine_forbids_rewind_and_replay() {
    let db = scoped_db();
    insert_exec(&db, E1, T1, H1, SHA, "proposed").expect("exec");
    assert!(
        set_exec_state(&db, E1, "completed").is_err(),
        "proposed -> completed skips running"
    );
    set_exec_state(&db, E1, "running").expect("running");
    assert!(set_exec_state(&db, E1, "proposed").is_err(), "no rewind");
    set_exec_state(&db, E1, "completed").expect("completed");
    assert!(
        set_exec_state(&db, E1, "running").is_err(),
        "terminal is final: no replay"
    );
}

#[test]
fn approval_is_single_use() {
    let db = scoped_db();
    insert_exec(&db, E1, T1, H1, SHA, "awaiting_approval").expect("exec");
    insert_approval(&db, A1, T1, E1, H1, SHA).expect("approval");
    set_approval_state(&db, A1, "approved").expect("approve");
    assert!(set_approval_state(&db, A1, "denied").is_err());
    assert!(set_approval_state(&db, A1, "pending").is_err());
}

// ---- keys and bounded previews ---------------------------------------------

#[test]
fn only_one_active_pinned_key_per_host_and_algorithm() {
    let db = scoped_db();
    let insert = |id: &str, replaced: Option<i64>| {
        db.conn().execute(
            "INSERT INTO known_host_keys(id, host_id, algorithm, public_key_base64,
               fingerprint_sha256, first_seen_at_ms, replaced_at_ms)
             VALUES (?1, ?2, 'ssh-ed25519', 'AAAA', 'SHA256:x', 1, ?3)",
            params![id, H1, replaced],
        )
    };
    insert("k1", None).expect("first key");
    assert!(
        insert("k2", None).is_err(),
        "second active key must be rejected"
    );
    insert("k3", Some(5)).expect("retired keys are kept as history");
}

#[test]
fn stdout_preview_is_bounded_in_bytes() {
    let db = scoped_db();
    insert_exec(&db, E1, T1, H1, SHA, "proposed").expect("exec");
    let big = "é".repeat(40_000); // 80 000 bytes, 40 000 chars
    assert!(db
        .conn()
        .execute(
            "UPDATE command_executions SET stdout_preview = ?1 WHERE id = ?2",
            params![big, E1],
        )
        .is_err());
    db.conn()
        .execute(
            "UPDATE command_executions SET stdout_preview = 'ok' WHERE id = ?1",
            params![E1],
        )
        .expect("small preview");
}

// ---- crash recovery --------------------------------------------------------

#[test]
fn restart_interrupts_live_work_without_replay() {
    let mut db = scoped_db();
    set_task_state(&db, T1, "validating_scope").expect("t");
    set_task_state(&db, T1, "ready").expect("t");
    set_task_state(&db, T1, "running").expect("t");
    insert_exec(&db, E1, T1, H1, SHA, "proposed").expect("e1");
    set_exec_state(&db, E1, "running").expect("e1 running");
    insert_exec(&db, E2, T1, H1, SHA, "awaiting_approval").expect("e2");
    insert_approval(&db, A1, T1, E2, H1, SHA).expect("approval");

    let report = db.reconcile_after_restart(T0 + 10).expect("reconcile");
    assert_eq!(report.tasks_interrupted, 1);
    assert_eq!(report.executions_interrupted, 2);
    assert_eq!(report.approvals_closed, 1);

    assert_eq!(
        scalar(
            &db,
            &format!("SELECT state FROM agent_tasks WHERE id = '{T1}'")
        ),
        "interrupted"
    );
    assert_eq!(
        scalar(
            &db,
            &format!("SELECT state FROM command_executions WHERE id = '{E1}'")
        ),
        "interrupted"
    );
    assert_eq!(
        scalar(
            &db,
            &format!("SELECT error_code FROM command_executions WHERE id = '{E1}'")
        ),
        "TASK_INTERRUPTED"
    );
    assert_eq!(
        scalar(
            &db,
            &format!("SELECT state FROM approval_requests WHERE id = '{A1}'")
        ),
        "cancelled"
    );

    // Interrupted is final: an interrupted command can never run again.
    assert!(set_exec_state(&db, E1, "running").is_err());
    assert!(set_task_state(&db, T1, "running").is_err());

    let again = db.reconcile_after_restart(T0 + 20).expect("idempotent");
    assert_eq!(again, Default::default());
}

#[test]
fn reconcile_leaves_terminal_history_untouched() {
    let mut db = scoped_db();
    insert_exec(&db, E1, T1, H1, SHA, "proposed").expect("exec");
    set_exec_state(&db, E1, "running").expect("run");
    set_exec_state(&db, E1, "completed").expect("done");
    db.conn()
        .execute(
            "UPDATE command_executions SET finished_at_ms = ?1 WHERE id = ?2",
            params![T0 + 5, E1],
        )
        .expect("finish time");

    db.reconcile_after_restart(T0 + 100).expect("reconcile");
    assert_eq!(
        scalar(
            &db,
            &format!("SELECT state FROM command_executions WHERE id = '{E1}'")
        ),
        "completed"
    );
    assert_eq!(
        scalar(
            &db,
            &format!("SELECT finished_at_ms FROM command_executions WHERE id = '{E1}'")
        ),
        (T0 + 5).to_string()
    );
}

#[test]
fn existing_v2_database_upgrades_in_place_and_keeps_its_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("v2.sqlite3");
    {
        // A database created by the previous release: migrations 1 and 2 only.
        let mut conn = rusqlite::Connection::open(&path).expect("open");
        conn.pragma_update(None, "foreign_keys", true).expect("fk");
        migrate::apply(&mut conn, &migrate::MIGRATIONS[..2], T0).expect("v2");
        conn.execute(
            "INSERT INTO hosts(id, label, hostname, username, auth_method, created_at_ms, updated_at_ms)
             VALUES ('00000000-0000-4000-8000-000000000001', 'legacy', 'host.example', 'ops', 'password', ?1, ?1)",
            params![T0],
        )
        .expect("host row");
    }

    let db = Database::open(&path).expect("upgrade on open");
    assert_eq!(db.schema_version().expect("version"), 11);
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM hosts WHERE kind = 'ssh'"),
        "1"
    );
    assert_eq!(
        scalar(&db, "SELECT label FROM hosts WHERE kind = 'ssh'"),
        "legacy"
    );
    let indexes = scalar(
        &db,
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'ix_commands_host_state'",
    );
    assert_eq!(indexes, "1");
}

#[test]
fn encrypted_memory_security_does_not_reenter_native_logging() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("memory-security.sqlite3");
    let key = [42; 32];
    // Exceed Windows' usual VirtualLock allowance so refusal to lock a page
    // cannot recursively invoke SQLCipher's allocator through its logger.
    let payload = vec![0x5a; 1024 * 1024];
    {
        let db = Database::open_encrypted(&path, &key).expect("encrypted open");
        assert_eq!(scalar(&db, "PRAGMA cipher_log_level"), "NONE");
        assert_eq!(scalar(&db, "PRAGMA cipher_memory_security"), "1");
        db.put_vault_secret("memory-security-test", &payload, false)
            .expect("write beyond locked-page allowance");
        assert_eq!(
            db.vault_secret("memory-security-test").expect("read"),
            Some(payload.clone())
        );
        db.checkpoint().expect("checkpoint");
        db.integrity_check().expect("integrity");
    }
    let db = Database::open_encrypted(&path, &key).expect("reopen encrypted");
    assert_eq!(
        db.vault_secret("memory-security-test")
            .expect("read after reopen"),
        Some(payload)
    );
    assert!(!std::fs::read(&path)
        .expect("ciphertext")
        .starts_with(b"SQLite format 3"));
}
