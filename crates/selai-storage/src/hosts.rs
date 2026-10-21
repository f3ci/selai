//! Host, group and pinned-key repositories (S-010, S-012).
//!
//! Inputs are already validated `selai-domain` types. Label uniqueness is
//! checked here for a clear error and enforced by the database index for races.

use rusqlite::{params, OptionalExtension, Row};
use selai_domain::{AuthMethod, GroupName, HostGroupId, HostId, HostSettings, Page, PageRequest};

use crate::{Database, StorageError};

/// A stored host as shown to the UI. Contains no secret material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRecord {
    pub id: HostId,
    pub label: String,
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    pub private_key_path: Option<String>,
    pub group_id: Option<HostGroupId>,
    pub connect_timeout_seconds: u16,
    pub keepalive_seconds: u16,
    pub terminal_type: String,
    pub notes: Option<String>,
    pub credential_configured: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_connected_at_ms: Option<i64>,
    pub archived_at_ms: Option<i64>,
    /// The built-in "This computer" target. It has no credentials, no host keys,
    /// and cannot be edited or archived.
    pub is_local: bool,
}

/// Fixed id of the built-in local target (migration 0005).
pub const LOCAL_HOST_ID: &str = "0c0c0c0c-0000-4000-8000-000000000000";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRecord {
    pub id: HostGroupId,
    pub name: String,
    pub sort_order: i64,
}

/// Result of recording a host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinOutcome {
    /// The same key was already pinned; nothing changed.
    Unchanged,
    /// First trust for this host and algorithm.
    Pinned,
    /// An explicit rotation replaced the previous key. The old row is kept.
    Rotated { previous_fingerprint: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedKey {
    pub algorithm: String,
    pub public_key_base64: String,
    pub fingerprint_sha256: String,
    pub first_seen_at_ms: i64,
}

const HOST_COLUMNS: &str = "id, label, hostname, port, username, COALESCE(auth_method_v2, auth_method), private_key_path,
    group_id, connect_timeout_seconds, keepalive_seconds, terminal_type, notes,
    credential_configured, created_at_ms, updated_at_ms, last_connected_at_ms, archived_at_ms, kind";

fn row_to_host(row: &Row<'_>) -> rusqlite::Result<HostRecord> {
    let id: String = row.get(0)?;
    let auth: String = row.get(5)?;
    let group: Option<String> = row.get(7)?;
    let port: i64 = row.get(3)?;
    let connect: i64 = row.get(8)?;
    let keepalive: i64 = row.get(9)?;
    let configured: i64 = row.get(12)?;
    Ok(HostRecord {
        id: id.parse().map_err(|e| to_sql_err(&e))?,
        label: row.get(1)?,
        hostname: row.get(2)?,
        port: u16::try_from(port).map_err(|e| to_sql_err(&e))?,
        username: row.get(4)?,
        auth: AuthMethod::parse(&auth).ok_or_else(|| to_sql_err(&"unknown auth_method"))?,
        private_key_path: row.get(6)?,
        group_id: group
            .map(|g| g.parse().map_err(|e| to_sql_err(&e)))
            .transpose()?,
        connect_timeout_seconds: u16::try_from(connect).map_err(|e| to_sql_err(&e))?,
        keepalive_seconds: u16::try_from(keepalive).map_err(|e| to_sql_err(&e))?,
        terminal_type: row.get(10)?,
        notes: row.get(11)?,
        credential_configured: configured == 1,
        created_at_ms: row.get(13)?,
        updated_at_ms: row.get(14)?,
        last_connected_at_ms: row.get(15)?,
        archived_at_ms: row.get(16)?,
        is_local: row.get::<_, String>(17)? == "local",
    })
}

fn to_sql_err(e: &dyn std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e.to_string(),
        )),
    )
}

impl Database {
    /// Moves a profile without reading or rewriting connection credentials.
    pub fn move_host_to_group(
        &mut self,
        id: HostId,
        group_id: Option<HostGroupId>,
        expected_updated_at_ms: i64,
        now_ms: i64,
    ) -> Result<HostRecord, StorageError> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: Option<(i64, Option<i64>)> = tx
            .query_row(
                "SELECT updated_at_ms, archived_at_ms FROM hosts WHERE id = ?1 AND kind = 'ssh'",
                params![id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (updated, archived) =
            current.ok_or_else(|| StorageError::NotFound(format!("host {id}")))?;
        if archived.is_some() || updated != expected_updated_at_ms {
            return Err(StorageError::Conflict(format!(
                "host {id} was changed elsewhere or archived"
            )));
        }
        if let Some(group) = group_id {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM host_groups WHERE id = ?1)",
                params![group.to_string()],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(StorageError::NotFound(format!("group {group}")));
            }
        }
        tx.execute("UPDATE hosts SET group_id = ?2, updated_at_ms = max(updated_at_ms + 1, ?3) WHERE id = ?1",
            params![id.to_string(), group_id.map(|group| group.to_string()), now_ms])?;
        tx.commit()?;
        self.get_host(id)?
            .ok_or_else(|| StorageError::NotFound(format!("host {id}")))
    }

    /// Creates an active host. Fails with `Conflict` if an active host already
    /// uses the label (case-insensitively).
    pub fn create_host(
        &mut self,
        id: HostId,
        settings: &HostSettings,
        now_ms: i64,
    ) -> Result<HostRecord, StorageError> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_label_free(&tx, settings.label.as_str(), None)?;
        tx.execute(
            "INSERT INTO hosts (id, label, hostname, port, username, auth_method, private_key_path,
                group_id, connect_timeout_seconds, keepalive_seconds, terminal_type, notes,
                credential_configured, created_at_ms, updated_at_ms, auth_method_v2)
             VALUES (?1, ?2, ?3, ?4, ?5, CASE WHEN ?6 = 'private_key' THEN 'private_key' ELSE 'password' END, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, ?13, ?6)",
            params![
                id.to_string(),
                settings.label.as_str(),
                settings.hostname.as_str(),
                i64::from(settings.port.get()),
                settings.username.as_str(),
                settings.auth.as_str(),
                settings.private_key_path.as_ref().map(|p| p.as_str()),
                settings.group_id.map(|g| g.to_string()),
                i64::from(settings.connect_timeout.seconds()),
                i64::from(settings.keepalive.seconds()),
                settings.terminal_type.as_str(),
                settings.notes.as_ref().map(|n| n.as_str()),
                now_ms,
            ],
        )?;
        tx.commit()?;
        self.get_host(id)?
            .ok_or_else(|| StorageError::NotFound(format!("host {id} after insert")))
    }

    /// Updates an active host if it still has `expected_updated_at_ms`.
    /// A different timestamp means someone else changed it: `Conflict`.
    pub fn update_host(
        &mut self,
        id: HostId,
        settings: &HostSettings,
        expected_updated_at_ms: i64,
        now_ms: i64,
    ) -> Result<HostRecord, StorageError> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: Option<(i64, Option<i64>)> = tx
            .query_row(
                "SELECT updated_at_ms, archived_at_ms FROM hosts WHERE id = ?1 AND kind = 'ssh'",
                params![id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (updated, archived) =
            current.ok_or_else(|| StorageError::NotFound(format!("host {id}")))?;
        if archived.is_some() {
            return Err(StorageError::Conflict(format!("host {id} is archived")));
        }
        if updated != expected_updated_at_ms {
            return Err(StorageError::Conflict(format!(
                "host {id} was changed elsewhere"
            )));
        }
        ensure_label_free(&tx, settings.label.as_str(), Some(id))?;
        tx.execute(
            "UPDATE hosts SET label = ?2, hostname = ?3, port = ?4, username = ?5, auth_method = CASE WHEN ?6 = 'private_key' THEN 'private_key' ELSE 'password' END, auth_method_v2 = ?6,
                private_key_path = ?7, group_id = ?8, connect_timeout_seconds = ?9,
                keepalive_seconds = ?10, terminal_type = ?11, notes = ?12, updated_at_ms = ?13
              WHERE id = ?1",
            params![
                id.to_string(),
                settings.label.as_str(),
                settings.hostname.as_str(),
                i64::from(settings.port.get()),
                settings.username.as_str(),
                settings.auth.as_str(),
                settings.private_key_path.as_ref().map(|p| p.as_str()),
                settings.group_id.map(|g| g.to_string()),
                i64::from(settings.connect_timeout.seconds()),
                i64::from(settings.keepalive.seconds()),
                settings.terminal_type.as_str(),
                settings.notes.as_ref().map(|n| n.as_str()),
                now_ms,
            ],
        )?;
        tx.commit()?;
        self.get_host(id)?
            .ok_or_else(|| StorageError::NotFound(format!("host {id} after update")))
    }

    /// Archives a host. History and pinned keys stay. Idempotent.
    pub fn archive_host(&mut self, id: HostId, now_ms: i64) -> Result<(), StorageError> {
        let changed = self.conn().execute(
            "UPDATE hosts SET archived_at_ms = ?2, updated_at_ms = ?2
              WHERE id = ?1 AND archived_at_ms IS NULL AND kind = 'ssh'",
            params![id.to_string(), now_ms],
        )?;
        if changed == 0 {
            match self.get_host(id)? {
                None => return Err(StorageError::NotFound(format!("host {id}"))),
                Some(h) if h.is_local => {
                    return Err(StorageError::Conflict(
                        "the local computer cannot be archived".to_owned(),
                    ))
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// The built-in local target.
    pub fn local_host(&self) -> Result<HostRecord, StorageError> {
        let id: HostId = LOCAL_HOST_ID
            .parse()
            .map_err(|_| StorageError::Constraint("invalid local host id".to_owned()))?;
        self.get_host(id)?
            .ok_or_else(|| StorageError::NotFound("the local computer entry".to_owned()))
    }

    pub fn get_host(&self, id: HostId) -> Result<Option<HostRecord>, StorageError> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {HOST_COLUMNS} FROM hosts WHERE id = ?1"),
                params![id.to_string()],
                row_to_host,
            )
            .optional()?)
    }

    /// Lists hosts ordered by case-folded label. The cursor is `label|id` of
    /// the last row returned. Labels cannot contain `|`, so the split is exact.
    pub fn list_hosts(
        &self,
        include_archived: bool,
        name_filter: Option<&str>,
        page: &PageRequest,
    ) -> Result<Page<HostRecord>, StorageError> {
        let (after_label, after_id) = match page.cursor.as_deref() {
            None => (String::new(), String::new()),
            Some(c) => {
                let (l, i) = c
                    .split_once('|')
                    .ok_or_else(|| StorageError::Constraint("malformed cursor".to_owned()))?;
                (l.to_owned(), i.to_owned())
            }
        };
        let pattern = name_filter
            .map(|f| format!("%{}%", escape_like(&f.to_ascii_lowercase())))
            .unwrap_or_else(|| "%".to_owned());
        let limit = i64::from(page.effective_limit());
        let sql = format!(
            "SELECT {HOST_COLUMNS} FROM hosts
              WHERE (?1 = 1 OR archived_at_ms IS NULL)
                AND kind = 'ssh'
                AND lower(label) LIKE ?2 ESCAPE '\\'
                AND (lower(label), id) > (?3, ?4)
              ORDER BY lower(label), id
              LIMIT ?5"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let items = stmt
            .query_map(
                params![
                    i64::from(include_archived),
                    pattern,
                    after_label,
                    after_id,
                    limit
                ],
                row_to_host,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if items.len() as i64 == limit {
            items
                .last()
                .map(|h| format!("{}|{}", h.label.to_ascii_lowercase(), h.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    pub fn set_credential_configured(
        &self,
        id: HostId,
        configured: bool,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let changed = self.conn().execute(
            "UPDATE hosts SET credential_configured = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id.to_string(), i64::from(configured), now_ms],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(format!("host {id}")));
        }
        Ok(())
    }

    pub fn mark_connected(&self, id: HostId, now_ms: i64) -> Result<(), StorageError> {
        self.conn().execute(
            "UPDATE hosts SET last_connected_at_ms = ?2 WHERE id = ?1",
            params![id.to_string(), now_ms],
        )?;
        Ok(())
    }

    pub fn create_group(
        &self,
        id: HostGroupId,
        name: &GroupName,
        sort_order: i64,
        now_ms: i64,
    ) -> Result<GroupRecord, StorageError> {
        self.ensure_group_name_free(name, None)?;
        self.conn().execute(
            "INSERT INTO host_groups (id, name, sort_order, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
            params![id.to_string(), name.as_str(), sort_order, now_ms],
        )?;
        Ok(GroupRecord {
            id,
            name: name.as_str().to_owned(),
            sort_order,
        })
    }

    pub fn list_groups(&self) -> Result<Vec<GroupRecord>, StorageError> {
        let mut stmt = self.conn().prepare(
            "SELECT id, name, sort_order FROM host_groups ORDER BY sort_order, lower(name)",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                Ok(GroupRecord {
                    id: id.parse().map_err(|e| to_sql_err(&e))?,
                    name: r.get(1)?,
                    sort_order: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn ensure_group_name_free(
        &self,
        name: &GroupName,
        except: Option<HostGroupId>,
    ) -> Result<(), StorageError> {
        let count: i64 = self.conn().query_row(
            "SELECT count(*) FROM host_groups WHERE name = ?1 COLLATE NOCASE AND (?2 IS NULL OR id != ?2)",
            params![name.as_str(), except.map(|id| id.to_string())], |r| r.get(0),
        )?;
        if count > 0 {
            return Err(StorageError::Conflict(
                "A group with this name already exists.".into(),
            ));
        }
        Ok(())
    }

    pub fn rename_group(
        &self,
        id: HostGroupId,
        name: &GroupName,
    ) -> Result<GroupRecord, StorageError> {
        self.ensure_group_name_free(name, Some(id))?;
        let changed = self.conn().execute(
            "UPDATE host_groups SET name = ?2 WHERE id = ?1",
            params![id.to_string(), name.as_str()],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(format!("group {id}")));
        }
        let sort_order = self.conn().query_row(
            "SELECT sort_order FROM host_groups WHERE id = ?1",
            params![id.to_string()],
            |r| r.get(0),
        )?;
        Ok(GroupRecord {
            id,
            name: name.as_str().into(),
            sort_order,
        })
    }

    /// Removing a group keeps its profiles and invalidates stale profile edits.
    pub fn delete_group(&mut self, id: HostGroupId, now_ms: i64) -> Result<(), StorageError> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("UPDATE hosts SET group_id = NULL, updated_at_ms = max(updated_at_ms + 1, ?2) WHERE group_id = ?1", params![id.to_string(), now_ms])?;
        if tx.execute(
            "DELETE FROM host_groups WHERE id = ?1",
            params![id.to_string()],
        )? == 0
        {
            return Err(StorageError::NotFound(format!("group {id}")));
        }
        tx.commit()?;
        Ok(())
    }

    /// Active pinned keys for a host, one per algorithm.
    pub fn active_pinned_keys(&self, host: HostId) -> Result<Vec<PinnedKey>, StorageError> {
        let mut stmt = self.conn().prepare(
            "SELECT algorithm, public_key_base64, fingerprint_sha256, first_seen_at_ms
               FROM known_host_keys WHERE host_id = ?1 AND replaced_at_ms IS NULL
              ORDER BY algorithm",
        )?;
        let rows = stmt
            .query_map(params![host.to_string()], |r| {
                Ok(PinnedKey {
                    algorithm: r.get(0)?,
                    public_key_base64: r.get(1)?,
                    fingerprint_sha256: r.get(2)?,
                    first_seen_at_ms: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// First trust. Pins a key only if the host has no active key for that
    /// algorithm. A different active key is an error: use [`Self::rotate_host_key`].
    pub fn pin_host_key(
        &mut self,
        host: HostId,
        key: &PinnedKey,
    ) -> Result<PinOutcome, StorageError> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT public_key_base64, fingerprint_sha256 FROM known_host_keys
                  WHERE host_id = ?1 AND algorithm = ?2 AND replaced_at_ms IS NULL",
                params![host.to_string(), key.algorithm],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let outcome =
            match existing {
                Some((material, _)) if material == key.public_key_base64 => PinOutcome::Unchanged,
                Some(_) => return Err(StorageError::Conflict(
                    "a different key is already pinned; rotation requires explicit confirmation"
                        .to_owned(),
                )),
                None => {
                    insert_key(&tx, host, key)?;
                    PinOutcome::Pinned
                }
            };
        tx.commit()?;
        Ok(outcome)
    }

    /// Explicit rotation: retires the active key for the algorithm (keeping it
    /// as history) and pins the new one in the same transaction.
    pub fn rotate_host_key(
        &mut self,
        host: HostId,
        key: &PinnedKey,
        now_ms: i64,
    ) -> Result<PinOutcome, StorageError> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT public_key_base64, fingerprint_sha256 FROM known_host_keys
                  WHERE host_id = ?1 AND algorithm = ?2 AND replaced_at_ms IS NULL",
                params![host.to_string(), key.algorithm],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let outcome = match previous {
            Some((material, _)) if material == key.public_key_base64 => PinOutcome::Unchanged,
            Some((_, previous_fingerprint)) => {
                tx.execute(
                    "UPDATE known_host_keys SET replaced_at_ms = ?3
                      WHERE host_id = ?1 AND algorithm = ?2 AND replaced_at_ms IS NULL",
                    params![host.to_string(), key.algorithm, now_ms],
                )?;
                insert_key(&tx, host, key)?;
                PinOutcome::Rotated {
                    previous_fingerprint,
                }
            }
            None => {
                insert_key(&tx, host, key)?;
                PinOutcome::Pinned
            }
        };
        tx.commit()?;
        Ok(outcome)
    }
}

fn insert_key(
    tx: &rusqlite::Transaction<'_>,
    host: HostId,
    key: &PinnedKey,
) -> Result<(), StorageError> {
    tx.execute(
        "INSERT INTO known_host_keys (id, host_id, algorithm, public_key_base64, fingerprint_sha256,
            first_seen_at_ms, replaced_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
        params![
            selai_domain::KnownKeyId::new().to_string(),
            host.to_string(),
            key.algorithm,
            key.public_key_base64,
            key.fingerprint_sha256,
            key.first_seen_at_ms,
        ],
    )?;
    Ok(())
}

fn ensure_label_free(
    tx: &rusqlite::Transaction<'_>,
    label: &str,
    except: Option<HostId>,
) -> Result<(), StorageError> {
    let taken: Option<String> = tx
        .query_row(
            "SELECT id FROM hosts WHERE archived_at_ms IS NULL AND label = ?1 COLLATE NOCASE",
            params![label],
            |r| r.get(0),
        )
        .optional()?;
    match (taken, except) {
        (Some(id), Some(me)) if id == me.to_string() => Ok(()),
        (Some(_), _) => Err(StorageError::Conflict(format!(
            "label '{label}' is already in use"
        ))),
        (None, _) => Ok(()),
    }
}

fn escape_like(input: &str) -> String {
    input
        .chars()
        .flat_map(|c| match c {
            '%' | '_' | '\\' => vec!['\\', c],
            other => vec![other],
        })
        .collect()
}
