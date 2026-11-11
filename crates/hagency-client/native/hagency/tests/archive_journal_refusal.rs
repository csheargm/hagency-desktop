//! Real normal personal-ledger opener must reject an archive refusal sentinel.
//! Only these synthetic fixtures create one; production adoption stays absent.
use hagency_agent_local::{Error, Ledger};
use hagency_store::private;
use rusqlite::Connection;

/// The application id of the archive files an earlier version wrote. Nothing
/// writes one any more; a file left on disk must still be refused untouched.
const MANAGEMENT_ARCHIVE_APPLICATION_ID: i32 = 0x48415231;

#[test]
fn archive_sentinel_blocks_absent_personal_ledger_initialization() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let profile = root.path().join("profile");
    private::directory(&profile).unwrap();
    let path = profile.join("agent-local.sqlite");
    private::open(&path, true).unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE adoption_refusal(singleton INTEGER PRIMARY KEY,manifest TEXT NOT NULL);INSERT INTO adoption_refusal VALUES(1,'{\"migrationId\":\"synthetic_only\",\"kind\":\"personal_refusal\"}');").unwrap();
    db.pragma_update(None, "application_id", MANAGEMENT_ARCHIVE_APPLICATION_ID)
        .unwrap();
    db.pragma_update(None, "user_version", 1).unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        Ledger::open_scoped(&path, "@owner:example.test", &"a".repeat(64)),
        Err(Error::ForeignDatabase)
    ));
    assert!(matches!(
        Ledger::open(&path, "@owner:example.test"),
        Err(Error::ForeignDatabase)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    assert!(db.prepare("SELECT owner FROM identity").is_err());
    drop(db);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "archive_refusal_child", "--nocapture"])
        .env("HAGENCY_ARCHIVE_REFUSAL_FIXTURE", &path)
        .output()
        .unwrap();
    assert!(output.status.success(), "normal opener child failed");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn archive_refusal_child() {
    hagency_platform::legacy_writer::test_support::initialize_process_root().unwrap();
    let Some(path) = std::env::var_os("HAGENCY_ARCHIVE_REFUSAL_FIXTURE") else {
        return;
    };
    assert!(matches!(
        Ledger::open_scoped(path, "@owner:example.test", &"a".repeat(64)),
        Err(Error::ForeignDatabase)
    ));
}

#[test]
fn archive_identity_refuses_existing_personal_ledger_without_recovery() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let path = root.path().join("agent-local.sqlite");
    private::open(&path, true).unwrap();
    drop(Ledger::open_scoped(&path, "@owner:example.test", &"a".repeat(64)).unwrap());
    let db = Connection::open(&path).unwrap();
    // Preserve an opaque original row. The test asserts normal open never
    // initializes or executes any special archive migration/recovery logic.
    db.execute_batch("CREATE TABLE original_private_evidence(value TEXT NOT NULL);INSERT INTO original_private_evidence VALUES('original');").unwrap();
    db.pragma_update(None, "application_id", MANAGEMENT_ARCHIVE_APPLICATION_ID)
        .unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        Ledger::open_scoped(&path, "@owner:example.test", &"a".repeat(64)),
        Err(Error::ForeignDatabase)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("SELECT value FROM original_private_evidence", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "original"
    );
}
