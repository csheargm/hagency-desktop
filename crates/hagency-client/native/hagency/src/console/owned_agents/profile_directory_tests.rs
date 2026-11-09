use super::*;

const ORIGIN: &str = "https://server.example/";
const ISSUER: &str = "https://server.example/auth/";
const OWNER: &str = "@alice:example";

fn directory(root: &Path, subject: &str) -> PathBuf {
    journal_profile_directory(root, ORIGIN, ISSUER, subject, OWNER).unwrap()
}

#[test]
fn journal_namespace_has_same_path_without_creating_any_database() {
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = temp.path().join("state");
    let path = directory(&root, "subject");
    assert_eq!(
        path,
        root.join("owned-agent-owners").join(format!(
            "owner_{}",
            profile_identity(ORIGIN, ISSUER, "subject", OWNER).unwrap()
        ))
    );
    assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
    assert!(path.join("profile-identity.json").is_file());
    assert!(!path.join("agent-local.sqlite").exists());
    hagency_store::private::write_new(&path.join("matrix-creations.json"), b"[]").unwrap();
    assert_eq!(directory(&root, "subject"), path);
    assert_eq!(std::fs::read_dir(&path).unwrap().count(), 2);
    let ledger = ledger_path(&root, ORIGIN, ISSUER, "subject", OWNER).unwrap();
    assert_eq!(ledger, path.join("agent-local.sqlite"));
}

#[test]
fn journal_namespace_does_not_open_or_migrate_existing_unknown_sqlite() {
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = temp.path().join("state");
    let path = directory(&root, "subject");
    let ledger = path.join("agent-local.sqlite");
    drop(hagency_store::private::open(&ledger, true).unwrap());
    let db = rusqlite::Connection::open(&ledger).unwrap();
    db.execute_batch("CREATE TABLE older_data(value TEXT); INSERT INTO older_data VALUES('retain'); PRAGMA user_version=1;").unwrap();
    let bytes = std::fs::read(&ledger).unwrap();
    // Journal access must not open the DB even when another writer owns it.
    db.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert_eq!(directory(&root, "subject"), path);
    assert_eq!(std::fs::read(&ledger).unwrap(), bytes);
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    drop(db);
    assert!(!path.join("agent-local.sqlite-wal").exists());
    assert!(!path.join("agent-local.sqlite-shm").exists());
    // Invalid bytes likewise remain an opaque unrelated file to the journal.
    hagency_store::private::replace(&ledger, b"not a database").unwrap();
    directory(&root, "subject");
    assert_eq!(std::fs::read(&ledger).unwrap(), b"not a database");
}

#[test]
fn journal_access_never_rewrites_legacy_marker_or_authorizes_foreign_copied_database() {
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = temp.path().join("state");
    let a = ledger_path(&root, ORIGIN, ISSUER, "subject-a", OWNER).unwrap();
    let b = directory(&root, "subject-b");
    let copied = b.join("agent-local.sqlite");
    std::fs::copy(a, &copied).unwrap();
    let marker = b.join("profile-identity.json");
    let legacy = LocalProfileIdentity {
        version: 1,
        origin: ORIGIN.into(),
        issuer: crate::server_admission::profile_storage_namespace(ORIGIN),
        subject: "subject-b".into(),
        owner: OWNER.into(),
    };
    let marker_bytes = serde_json::to_vec(&legacy).unwrap();
    hagency_store::private::replace(&marker, &marker_bytes).unwrap();
    let journal = b.join("matrix-creations.json");
    hagency_store::private::write_new(&journal, b"[]").unwrap();
    let original = std::fs::read(&copied).unwrap();
    assert!(journal_profile_directory(&root, ORIGIN, ISSUER, "subject-b", OWNER).is_err());
    assert_eq!(std::fs::read(&copied).unwrap(), original);
    assert_eq!(std::fs::read(&marker).unwrap(), marker_bytes);
    assert_eq!(std::fs::read(&journal).unwrap(), b"[]");
    assert!(ledger_path(&root, ORIGIN, ISSUER, "subject-b", OWNER).is_err());
    assert_eq!(std::fs::read(&marker).unwrap(), marker_bytes);
    assert!(
        Ledger::open_scoped(
            &copied,
            OWNER,
            &profile_identity(ORIGIN, ISSUER, "subject-b", OWNER).unwrap()
        )
        .is_err()
    );
}

#[test]
fn journal_namespace_refuses_missing_marker_in_nonempty_directory_and_wrong_identity() {
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = temp.path().join("state");
    let path = directory(&root, "subject");
    let marker = path.join("profile-identity.json");
    hagency_store::private::write_new(&path.join("matrix-creations.json"), b"[]").unwrap();
    std::fs::remove_file(&marker).unwrap();
    assert!(journal_profile_directory(&root, ORIGIN, ISSUER, "subject", OWNER).is_err());
    assert!(!marker.exists());
    let wrong = LocalProfileIdentity {
        version: 1,
        origin: ORIGIN.into(),
        issuer: ISSUER.into(),
        subject: "other-subject".into(),
        owner: OWNER.into(),
    };
    hagency_store::private::write_new(&marker, &serde_json::to_vec(&wrong).unwrap()).unwrap();
    assert!(journal_profile_directory(&root, ORIGIN, ISSUER, "subject", OWNER).is_err());
    assert!(!path.join("agent-local.sqlite").exists());
}
