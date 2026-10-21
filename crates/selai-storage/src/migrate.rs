use rusqlite::{params, Connection, TransactionBehavior};

use crate::StorageError;

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/0001_initial.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/0002_invariants.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("../migrations/0003_history.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("../migrations/0004_task_usage.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("../migrations/0005_local_host.sql"),
    },
    Migration {
        version: 6,
        sql: include_str!("../migrations/0006_chat_sessions.sql"),
    },
    Migration {
        version: 7,
        sql: include_str!("../migrations/0007_vault_credentials.sql"),
    },
    Migration {
        version: 8,
        sql: include_str!("../migrations/0008_ssh_keys.sql"),
    },
    Migration {
        version: 9,
        sql: include_str!("../migrations/0009_network_profiles.sql"),
    },
    Migration {
        version: 10,
        sql: include_str!("../migrations/0010_authentication.sql"),
    },
];

/// Applies every migration newer than the database's recorded version.
///
/// Each migration runs in its own immediate transaction together with its
/// bookkeeping row, so a failed step leaves the schema at the previous version.
/// Returns the version the database is at afterwards.
pub(crate) fn apply(
    conn: &mut Connection,
    migrations: &[Migration],
    now_ms: i64,
) -> Result<i64, StorageError> {
    if migrations.windows(2).any(|w| w[0].version >= w[1].version) {
        return Err(StorageError::Migration {
            version: 0,
            message: "migration versions must be strictly increasing".to_owned(),
        });
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
           version INTEGER PRIMARY KEY,
           applied_at_ms INTEGER NOT NULL
         );",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    let latest = migrations.last().map_or(0, |m| m.version);
    if current > latest {
        return Err(StorageError::SchemaTooNew {
            found: current,
            supported: latest,
        });
    }
    for migration in migrations.iter().filter(|m| m.version > current) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(migration.sql)
            .map_err(|err| StorageError::Migration {
                version: migration.version,
                message: err.to_string(),
            })?;
        tx.execute(
            "INSERT INTO schema_migrations(version, applied_at_ms) VALUES (?1, ?2)",
            params![migration.version, now_ms],
        )?;
        tx.commit()?;
    }
    Ok(latest)
}
