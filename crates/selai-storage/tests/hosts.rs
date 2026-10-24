#![allow(clippy::expect_used, clippy::unwrap_used)] // tests may panic on setup failure
//! Host repository behaviour through the public API.

use selai_domain::{
    AuthMethod, ConnectTimeout, GroupName, HostGroupId, HostId, HostLabel, HostSettings, Hostname,
    Keepalive, KeyPath, PageRequest, Port, TerminalType, Username,
};
use selai_storage::{Database, PinOutcome, PinnedKey, StorageError};

const T0: i64 = 1_000;

#[test]
fn wsl_distributions_have_stable_distinct_local_identities() {
    let mut db = Database::open_in_memory().expect("db");
    let native = db.local_host().expect("native host");
    let ubuntu = db.ensure_wsl_host("Ubuntu-24.04").expect("Ubuntu");
    let repeated = db.ensure_wsl_host("ubuntu-24.04").expect("same distro");
    let debian = db.ensure_wsl_host("Debian").expect("Debian");
    assert_eq!(ubuntu.id, repeated.id);
    assert_ne!(ubuntu.id, native.id);
    assert_ne!(ubuntu.id, debian.id);
    assert!(ubuntu.is_local);
    assert_eq!(ubuntu.wsl_distribution.as_deref(), Some("Ubuntu-24.04"));
    assert!(!ubuntu.credential_configured);
    assert!(db
        .list_hosts(false, None, &PageRequest::default())
        .expect("SSH hosts")
        .items
        .is_empty());
    assert!(db.archive_host(ubuntu.id, T0).is_err());
    assert!(db
        .update_host(ubuntu.id, &settings("Renamed", AuthMethod::Password), 0, T0)
        .is_err());
}

#[test]
fn wsl_identity_survives_reopening_and_rejects_invalid_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("wsl.sqlite3");
    let id = {
        let mut db = Database::open(&path).expect("db");
        db.ensure_wsl_host("Debian").expect("distro").id
    };
    let mut db = Database::open(&path).expect("reopen");
    assert_eq!(db.ensure_wsl_host("Debian").expect("distro").id, id);
    for bad in ["", "--help", "invalid\nname", "invalid|name"] {
        assert!(db.ensure_wsl_host(bad).is_err());
    }
}

fn settings(label: &str, auth: AuthMethod) -> HostSettings {
    HostSettings {
        label: HostLabel::parse(label).expect("label"),
        hostname: Hostname::parse("web.example.com").expect("host"),
        port: Port::SSH_DEFAULT,
        username: Username::parse("deploy").expect("user"),
        auth,
        private_key_path: match auth {
            AuthMethod::PrivateKey => {
                Some(KeyPath::parse("/home/op/.ssh/id_ed25519").expect("path"))
            }
            _ => None,
        },
        group_id: None,
        connect_timeout: ConnectTimeout::DEFAULT,
        keepalive: Keepalive::DEFAULT,
        terminal_type: TerminalType::parse(TerminalType::DEFAULT).expect("term"),
        notes: None,
    }
}

fn key(material: &str, fp: &str) -> PinnedKey {
    PinnedKey {
        algorithm: "ssh-ed25519".to_owned(),
        public_key_base64: material.to_owned(),
        fingerprint_sha256: fp.to_owned(),
        first_seen_at_ms: T0,
    }
}

#[test]
fn create_and_read_back_without_any_secret_field() {
    let mut db = Database::open_in_memory().expect("db");
    let id = HostId::new();
    let rec = db
        .create_host(id, &settings("production-web", AuthMethod::Password), T0)
        .expect("create");
    assert_eq!(rec.id, id);
    assert_eq!(rec.label, "production-web");
    assert_eq!(rec.port, 22);
    assert!(!rec.credential_configured, "no secret yet");
    assert_eq!(db.get_host(id).expect("get"), Some(rec));
}

#[test]
fn labels_conflict_case_insensitively_among_active_hosts() {
    let mut db = Database::open_in_memory().expect("db");
    db.create_host(
        HostId::new(),
        &settings("production-web", AuthMethod::Password),
        T0,
    )
    .expect("first");
    let err = db
        .create_host(
            HostId::new(),
            &settings("PRODUCTION-WEB", AuthMethod::Password),
            T0,
        )
        .expect_err("case variant must conflict");
    assert!(matches!(err, StorageError::Conflict(_)), "{err:?}");
}

#[test]
fn archived_label_is_free_and_history_keeps_the_row() {
    let mut db = Database::open_in_memory().expect("db");
    let old = HostId::new();
    db.create_host(old, &settings("staging", AuthMethod::Password), T0)
        .expect("create");
    db.archive_host(old, T0 + 1).expect("archive");
    db.archive_host(old, T0 + 2).expect("archive is idempotent");
    let archived = db.get_host(old).expect("get").expect("row kept");
    assert_eq!(archived.archived_at_ms, Some(T0 + 1));

    db.create_host(
        HostId::new(),
        &settings("staging", AuthMethod::Password),
        T0 + 3,
    )
    .expect("label reusable after archive");
}

#[test]
fn rename_keeps_identity_and_frees_old_label() {
    let mut db = Database::open_in_memory().expect("db");
    let id = HostId::new();
    let rec = db
        .create_host(id, &settings("web-a", AuthMethod::Password), T0)
        .expect("create");
    let renamed = db
        .update_host(
            id,
            &settings("web-b", AuthMethod::Password),
            rec.updated_at_ms,
            T0 + 5,
        )
        .expect("rename");
    assert_eq!(renamed.id, id, "identity never changes");
    assert_eq!(renamed.label, "web-b");
    db.create_host(
        HostId::new(),
        &settings("web-a", AuthMethod::Password),
        T0 + 6,
    )
    .expect("old label is free");
}

#[test]
fn update_with_stale_version_is_a_conflict() {
    let mut db = Database::open_in_memory().expect("db");
    let id = HostId::new();
    let rec = db
        .create_host(id, &settings("web", AuthMethod::Password), T0)
        .expect("create");
    db.update_host(
        id,
        &settings("web", AuthMethod::Password),
        rec.updated_at_ms,
        T0 + 1,
    )
    .expect("first writer");
    let stale = db.update_host(
        id,
        &settings("web", AuthMethod::Password),
        rec.updated_at_ms,
        T0 + 2,
    );
    assert!(matches!(stale, Err(StorageError::Conflict(_))));
}

#[test]
fn archived_hosts_cannot_be_edited() {
    let mut db = Database::open_in_memory().expect("db");
    let id = HostId::new();
    let rec = db
        .create_host(id, &settings("web", AuthMethod::Password), T0)
        .expect("create");
    db.archive_host(id, T0 + 1).expect("archive");
    let err = db.update_host(
        id,
        &settings("web", AuthMethod::Password),
        rec.updated_at_ms,
        T0 + 2,
    );
    assert!(matches!(err, Err(StorageError::Conflict(_))));
}

#[test]
fn list_is_ordered_paged_and_filterable() {
    let mut db = Database::open_in_memory().expect("db");
    for name in ["delta", "alpha", "charlie", "bravo", "echo"] {
        db.create_host(HostId::new(), &settings(name, AuthMethod::Password), T0)
            .expect(name);
    }
    let first = db
        .list_hosts(false, None, &PageRequest::first(2))
        .expect("page 1");
    let labels: Vec<_> = first.items.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(labels, ["alpha", "bravo"]);
    let cursor = first.next_cursor.clone().expect("more pages");

    let second = db
        .list_hosts(
            false,
            None,
            &PageRequest {
                cursor: Some(cursor),
                limit: 2,
            },
        )
        .expect("page 2");
    let labels: Vec<_> = second.items.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(labels, ["charlie", "delta"]);

    let filtered = db
        .list_hosts(false, Some("har"), &PageRequest::first(10))
        .expect("filter");
    assert_eq!(filtered.items.len(), 1);
    assert_eq!(filtered.items[0].label, "charlie");
    assert!(filtered.next_cursor.is_none());
}

#[test]
fn filter_treats_wildcards_literally() {
    let mut db = Database::open_in_memory().expect("db");
    db.create_host(HostId::new(), &settings("web-1", AuthMethod::Password), T0)
        .expect("a");
    db.create_host(HostId::new(), &settings("webx", AuthMethod::Password), T0)
        .expect("b");
    let hits = db
        .list_hosts(false, Some("web_"), &PageRequest::first(10))
        .expect("q");
    assert!(
        hits.items.is_empty(),
        "underscore must not match any character"
    );
}

#[test]
fn archived_hosts_are_hidden_unless_requested() {
    let mut db = Database::open_in_memory().expect("db");
    let id = HostId::new();
    db.create_host(id, &settings("old", AuthMethod::Password), T0)
        .expect("c");
    db.archive_host(id, T0 + 1).expect("archive");
    assert!(db
        .list_hosts(false, None, &PageRequest::first(10))
        .expect("l")
        .items
        .is_empty());
    assert_eq!(
        db.list_hosts(true, None, &PageRequest::first(10))
            .expect("l")
            .items
            .len(),
        1
    );
}

#[test]
fn malformed_cursor_is_rejected() {
    let db = Database::open_in_memory().expect("db");
    let err = db.list_hosts(
        false,
        None,
        &PageRequest {
            cursor: Some("nope".into()),
            limit: 5,
        },
    );
    assert!(err.is_err());
}

#[test]
fn credential_flag_and_last_connected_are_updated() {
    let mut db = Database::open_in_memory().expect("db");
    let id = HostId::new();
    db.create_host(id, &settings("web", AuthMethod::Password), T0)
        .expect("c");
    db.set_credential_configured(id, true, T0 + 1)
        .expect("flag");
    db.mark_connected(id, T0 + 2).expect("connected");
    let rec = db.get_host(id).expect("g").expect("row");
    assert!(rec.credential_configured);
    assert_eq!(rec.last_connected_at_ms, Some(T0 + 2));
    assert!(matches!(
        db.set_credential_configured(HostId::new(), true, T0),
        Err(StorageError::NotFound(_))
    ));
}

#[test]
fn groups_are_listed_in_order_and_hosts_reference_them() {
    let mut db = Database::open_in_memory().expect("db");
    let g = HostGroupId::new();
    db.create_group(g, &GroupName::parse("Production").expect("name"), 2, T0)
        .expect("g");
    db.create_group(
        HostGroupId::new(),
        &GroupName::parse("Lab").expect("name"),
        1,
        T0,
    )
    .expect("g2");
    let names: Vec<_> = db
        .list_groups()
        .expect("list")
        .into_iter()
        .map(|g| g.name)
        .collect();
    assert_eq!(names, ["Lab", "Production"]);

    let mut s = settings("web", AuthMethod::Password);
    s.group_id = Some(g);
    let rec = db
        .create_host(HostId::new(), &s, T0)
        .expect("host in group");
    assert_eq!(rec.group_id, Some(g));

    let mut dangling = settings("web2", AuthMethod::Password);
    dangling.group_id = Some(HostGroupId::new());
    assert!(
        db.create_host(HostId::new(), &dangling, T0).is_err(),
        "unknown group rejected"
    );
}

#[test]
fn renaming_and_deleting_groups_keep_hosts_and_prevent_stale_edits() {
    let mut db = Database::open_in_memory().expect("db");
    let id = HostGroupId::new();
    db.create_group(id, &GroupName::parse("Production").expect("name"), 0, T0)
        .expect("group");
    assert!(matches!(
        db.create_group(
            HostGroupId::new(),
            &GroupName::parse("production").expect("name"),
            0,
            T0
        ),
        Err(StorageError::Conflict(_))
    ));
    let mut s = settings("web", AuthMethod::Password);
    s.group_id = Some(id);
    let host = db.create_host(HostId::new(), &s, T0).expect("host");
    db.rename_group(id, &GroupName::parse("Servers").expect("name"))
        .expect("rename");
    assert_eq!(
        db.get_host(host.id).expect("read").expect("host").group_id,
        Some(id)
    );
    assert_eq!(db.list_groups().expect("groups")[0].name, "Servers");
    db.delete_group(id, T0 + 1).expect("delete group");
    let remaining = db.get_host(host.id).expect("read").expect("host kept");
    assert!(remaining.group_id.is_none());
    assert!(remaining.archived_at_ms.is_none());
    assert!(remaining.updated_at_ms > host.updated_at_ms);
    s.group_id = None;
    assert!(matches!(
        db.update_host(host.id, &s, host.updated_at_ms, T0 + 2),
        Err(StorageError::Conflict(_))
    ));
    assert!(db.list_groups().expect("groups").is_empty());
    assert!(matches!(
        db.delete_group(id, T0 + 2),
        Err(StorageError::NotFound(_))
    ));
}

#[test]
fn first_trust_pins_and_same_key_is_unchanged() {
    let mut db = Database::open_in_memory().expect("db");
    let id = HostId::new();
    db.create_host(id, &settings("web", AuthMethod::Password), T0)
        .expect("c");
    let k = key("AAAA-one", "SHA256:one");
    assert_eq!(db.pin_host_key(id, &k).expect("pin"), PinOutcome::Pinned);
    assert_eq!(
        db.pin_host_key(id, &k).expect("again"),
        PinOutcome::Unchanged
    );
    assert_eq!(db.active_pinned_keys(id).expect("keys").len(), 1);
}

#[test]
fn a_different_key_is_never_pinned_silently() {
    let mut db = Database::open_in_memory().expect("db");
    let id = HostId::new();
    db.create_host(id, &settings("web", AuthMethod::Password), T0)
        .expect("c");
    db.pin_host_key(id, &key("AAAA-one", "SHA256:one"))
        .expect("pin");
    let err = db.pin_host_key(id, &key("AAAA-two", "SHA256:two"));
    assert!(matches!(err, Err(StorageError::Conflict(_))), "{err:?}");
    assert_eq!(
        db.active_pinned_keys(id).expect("keys")[0].fingerprint_sha256,
        "SHA256:one"
    );
}

#[test]
fn explicit_rotation_keeps_the_old_key_as_history() {
    let mut db = Database::open_in_memory().expect("db");
    let id = HostId::new();
    db.create_host(id, &settings("web", AuthMethod::Password), T0)
        .expect("c");
    db.pin_host_key(id, &key("AAAA-one", "SHA256:one"))
        .expect("pin");
    let outcome = db
        .rotate_host_key(id, &key("AAAA-two", "SHA256:two"), T0 + 10)
        .expect("rotate");
    assert_eq!(
        outcome,
        PinOutcome::Rotated {
            previous_fingerprint: "SHA256:one".into()
        }
    );

    let active = db.active_pinned_keys(id).expect("keys");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].fingerprint_sha256, "SHA256:two");
    assert_eq!(
        db.retired_key_count(id).expect("history"),
        1,
        "old key retained"
    );
}

#[test]
fn labels_with_spaces_keep_their_case_and_still_conflict_ignoring_case() {
    let mut db = Database::open_in_memory().expect("db");
    let id = HostId::new();
    db.create_host(id, &settings("Production Web", AuthMethod::Password), T0)
        .expect("create");
    let stored = db.get_host(id).expect("get").expect("row");
    assert_eq!(stored.label, "Production Web", "display case is kept");

    // Same label with other case and extra spaces: HostLabel collapses the spaces,
    // and the NOCASE index refuses it.
    let err = db
        .create_host(
            HostId::new(),
            &settings("  production   WEB ", AuthMethod::Password),
            T0,
        )
        .unwrap_err();
    assert!(matches!(err, StorageError::Conflict(_)), "{err:?}");
}

#[test]
fn the_local_computer_is_built_in_unlisted_and_read_only() {
    let mut db = Database::open_in_memory().expect("db");
    let local = db.local_host().expect("local host");
    assert!(local.is_local);
    assert_eq!(local.label, "This computer");
    assert_eq!(local.id.to_string(), selai_storage::LOCAL_HOST_ID);

    let listed = db
        .list_hosts(true, None, &PageRequest::first(50))
        .expect("list");
    assert!(
        listed.items.is_empty(),
        "SSH host listings never include it"
    );

    let edit = db.update_host(
        local.id,
        &settings("renamed", AuthMethod::Password),
        local.updated_at_ms,
        1,
    );
    assert!(matches!(edit, Err(StorageError::NotFound(_))));
    assert!(matches!(
        db.archive_host(local.id, 1),
        Err(StorageError::Conflict(_))
    ));
    assert!(!db
        .local_host()
        .expect("still there")
        .archived_at_ms
        .is_some());

    // Its label is taken, like any active host's.
    let clash = db.create_host(
        HostId::new(),
        &settings("this computer", AuthMethod::Password),
        1,
    );
    assert!(matches!(clash, Err(StorageError::Conflict(_))));
}

#[test]
fn advanced_authentication_roundtrips_without_changing_host_identity_or_pins() {
    let mut db = Database::open_in_memory().unwrap();
    let id = HostId::new();
    let mut host = db
        .create_host(id, &settings("Authentication", AuthMethod::Password), T0)
        .unwrap();
    let pin = key("AAAA", "SHA256:test");
    db.pin_host_key(id, &pin).unwrap();
    for (index, method) in [
        AuthMethod::Auto,
        AuthMethod::KeyboardInteractive,
        AuthMethod::Agent,
        AuthMethod::Certificate,
        AuthMethod::SecurityKey,
        AuthMethod::SmartCard,
        AuthMethod::Gssapi,
        AuthMethod::HostBased,
        AuthMethod::PrivateKey,
        AuthMethod::Password,
    ]
    .into_iter()
    .enumerate()
    {
        let mut input = settings("Authentication", method);
        if method == AuthMethod::Certificate {
            input.private_key_path = Some(KeyPath::parse("/home/op/.ssh/cert_identity").unwrap());
        }
        host = db
            .update_host(id, &input, host.updated_at_ms, T0 + index as i64 + 1)
            .unwrap();
        assert_eq!(db.get_host(id).unwrap().unwrap().auth, method);
        assert_eq!(db.active_pinned_keys(id).unwrap(), vec![pin.clone()]);
    }
}
