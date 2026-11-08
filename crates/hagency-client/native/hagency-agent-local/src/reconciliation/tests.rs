use super::*;
use std::sync::atomic::AtomicUsize;

const OWNER: &str = "@owner:example.test";
fn profile() -> String {
    "a".repeat(64)
}
fn authority() -> PersonalAuditAuthority {
    PersonalAuditAuthority::new(
        OWNER.into(),
        profile(),
        Instant::now() + Duration::from_secs(30),
        || true,
    )
    .unwrap()
}
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, Ledger) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("agent-local.sqlite");
    let ledger = Ledger::open_scoped(&path, OWNER, &profile()).unwrap();
    (root, path, ledger)
}
fn usage(version: &str, input: u64) -> String {
    serde_json::to_string(&Usage {
        input,
        output: 0,
        cached_input: 0,
        cache_write_input: None,
        reasoning_output: 0,
        accounting_version: version.into(),
    })
    .unwrap()
}
fn scope() -> Scope {
    Scope {
        agent: "private-agent".into(),
        binding: "private-binding".into(),
        room: "!secret-room:example.test".into(),
        requester: "@secret-user:example.test".into(),
        thread: "secret-thread".into(),
    }
}
fn bind(l: &mut Ledger) {
    l.register_binding(OWNER, &scope()).unwrap();
}
fn call(l: &Ledger, id: &str, state: &str, reserved: u64, value: Option<String>) {
    l.db.execute(
        "INSERT INTO calls VALUES (?1,'private-binding',?2,'private-digest',?3,'[]',?4,?5)",
        (id, json(&scope()).unwrap(), reserved, state, value),
    )
    .unwrap();
}
fn inbox_row(l: &Ledger, id: &str, state: &str) {
    l.db.execute("INSERT INTO local_inbox VALUES (?1,'private-binding',?1,'private-digest','secret prompt and message',?2,?1,NULL,NULL,1)", (id,state)).unwrap();
}
fn snapshot(l: &Ledger) -> Vec<(String, i64)> {
    l.db.prepare("SELECT name,(SELECT COUNT(*) FROM calls) FROM sqlite_schema ORDER BY name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
}

#[test]
fn personal_audit_retains_pending_unknown_and_private_evidence() {
    let (_root, _path, mut ledger) = fixture();
    bind(&mut ledger);
    call(&ledger, "pending-private", "pending", 500, None);
    call(&ledger, "unknown-private", "unknown", 400, None);
    call(
        &ledger,
        "reported-private",
        "settled",
        50,
        Some(usage("claude-reported-v1", 25)),
    );
    for state in [
        "prepared",
        "running",
        "unknown",
        "reply_ready",
        "failed",
        "received",
    ] {
        inbox_row(&ledger, state, state);
    }
    for layer in ["agent", "room", "requester"] {
        ledger
            .db
            .execute(
                "INSERT INTO accounts VALUES (?1,'lifetime',25,900)",
                [layer],
            )
            .unwrap();
    }
    ledger.db.execute("INSERT INTO approvals VALUES ('private-approval','private-binding','secret-tool-body',9999,0)", []).unwrap();
    ledger.db.execute("INSERT INTO model_profiles VALUES ('private-agent','secret-model','keychain:secret','/secret/workspace','high',1)", []).unwrap();
    let before = snapshot(&ledger);
    let audit = ledger.reconciliation_audit(&authority()).unwrap();
    assert_eq!(audit.adoption, "not_assessed");
    assert_eq!(audit.history.pending_calls, 1);
    assert_eq!(audit.history.unknown_calls, 1);
    assert_eq!(audit.history.active_executions, 2);
    assert_eq!(audit.history.unknown_executions, 1);
    assert_eq!(audit.history.positive_held_rows, 3);
    assert_eq!(audit.history.positive_spent_rows, 3);
    assert!(
        audit
            .blockers
            .contains(&PersonalAuditBlocker::UncertainExecutions)
    );
    assert!(
        audit
            .blockers
            .contains(&PersonalAuditBlocker::RetainedAccounting)
    );
    assert_eq!(before, snapshot(&ledger));
    let public = serde_json::to_string(&audit).unwrap();
    for private in [
        "secret-",
        "private-",
        "keychain:",
        "/secret",
        "local_inbox",
        "credential_ref",
        "providerTokens",
        "heldTokens",
        "spentTokens",
    ] {
        assert!(!public.contains(private), "{private}");
    }
    assert!(public.len() < 4096);
    let states: Vec<String> = ledger
        .db
        .prepare("SELECT state FROM calls ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(states, ["pending", "settled", "unknown"]);
}

#[test]
fn personal_audit_recovery_preserves_unknown_usage_and_reservation() {
    let (_root, _path, mut ledger) = fixture();
    bind(&mut ledger);
    call(&ledger, "interrupted", "pending", 700, None);
    // Explicit recovery is fixture setup only; the audit must not call it.
    ledger.recover_interrupted(OWNER).unwrap();
    call(
        &ledger,
        "reported-zero",
        "settled",
        0,
        Some(usage("claude-reported-v1", 0)),
    );
    call(
        &ledger,
        "no-provider",
        "settled",
        0,
        Some(usage("no-provider-call-v1", 0)),
    );
    let before = snapshot(&ledger);
    let audit = ledger.reconciliation_audit(&authority()).unwrap();
    assert_eq!(audit.history.pending_calls, 0);
    assert_eq!(audit.history.unreported_zero_usage, 0);
    assert_eq!(audit.history.unknown_calls, 1);
    assert_eq!(audit.history.missing_or_invalid_usage, 1);
    assert_eq!(audit.history.reported_usage_records, 1);
    assert_eq!(audit.history.no_provider_usage_records, 1);
    assert!(audit.history.explicit_usage_gaps > 0);
    assert!(
        audit
            .blockers
            .contains(&PersonalAuditBlocker::MissingOrUnreportedUsage)
    );
    assert!(
        audit
            .blockers
            .contains(&PersonalAuditBlocker::ExecutionHistory)
    );
    assert_eq!(before, snapshot(&ledger));
}

#[test]
fn personal_audit_reads_committed_wal_without_changing_main_or_wal() {
    let (_root, path, mut ledger) = fixture();
    bind(&mut ledger);
    ledger
        .db
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    ledger
        .db
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    let writer = Connection::open(&path).unwrap();
    writer
        .execute(
            "INSERT INTO accounts VALUES ('private','lifetime',123,456)",
            [],
        )
        .unwrap();
    let wal = std::path::PathBuf::from(format!("{}-wal", path.display()));
    let main_before = std::fs::read(&path).unwrap();
    let wal_before = std::fs::read(&wal).unwrap();
    assert!(!wal_before.is_empty());
    let rows_before = snapshot(&ledger);
    let audit = ledger.reconciliation_audit(&authority()).unwrap();
    assert_eq!(audit.history.positive_held_rows, 1);
    assert_eq!(audit.history.positive_spent_rows, 1);
    assert_eq!(main_before, std::fs::read(&path).unwrap());
    assert_eq!(wal_before, std::fs::read(&wal).unwrap());
    assert_eq!(rows_before, snapshot(&ledger));
    assert!(ledger.db.is_autocommit());
    drop(writer);
}

#[test]
fn personal_audit_authority_and_existing_transaction_fail_closed() {
    let (_root, path, mut ledger) = fixture();
    let before = std::fs::read(&path).unwrap();
    let wrong_owner = PersonalAuditAuthority::new(
        "@other:example.test".into(),
        profile(),
        Instant::now() + Duration::from_secs(10),
        || true,
    )
    .unwrap();
    assert!(matches!(
        ledger.reconciliation_audit(&wrong_owner),
        Err(Error::Unauthorized)
    ));
    let wrong_profile = PersonalAuditAuthority::new(
        OWNER.into(),
        "b".repeat(64),
        Instant::now() + Duration::from_secs(10),
        || true,
    )
    .unwrap();
    assert!(matches!(
        ledger.reconciliation_audit(&wrong_profile),
        Err(Error::Unauthorized)
    ));
    let closed = authority();
    closed.retire();
    assert!(matches!(
        ledger.reconciliation_audit(&closed),
        Err(Error::Unauthorized)
    ));
    assert!(
        PersonalAuditAuthority::new(
            OWNER.into(),
            profile(),
            Instant::now() - Duration::from_secs(1),
            || true
        )
        .is_err()
    );
    ledger.db.execute_batch("BEGIN DEFERRED").unwrap();
    assert!(matches!(
        ledger.reconciliation_audit(&authority()),
        Err(Error::Conflict)
    ));
    assert!(!ledger.db.is_autocommit());
    ledger.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(before, std::fs::read(path).unwrap());
}

#[test]
fn personal_audit_live_gate_is_rechecked_during_snapshot() {
    let (_root, path, mut ledger) = fixture();
    let before = std::fs::read(&path).unwrap();
    let checks = Arc::new(AtomicUsize::new(0));
    let gate = checks.clone();
    let proof = PersonalAuditAuthority::new(
        OWNER.into(),
        profile(),
        Instant::now() + Duration::from_secs(30),
        move || gate.fetch_add(1, Ordering::AcqRel) < 8,
    )
    .unwrap();
    assert!(matches!(
        ledger.reconciliation_audit(&proof),
        Err(Error::Unauthorized)
    ));
    assert!(checks.load(Ordering::Acquire) >= 9);
    assert!(ledger.db.is_autocommit());
    assert_eq!(before, std::fs::read(path).unwrap());
}

#[test]
fn personal_audit_bounds_and_malformed_history_are_explicit() {
    let (_root, _path, mut ledger) = fixture();
    bind(&mut ledger);
    call(
        &ledger,
        "oversized",
        "settled",
        0,
        Some("x".repeat(MAX_USAGE_BYTES as usize + 1)),
    );
    call(
        &ledger,
        "unknown-mode",
        "settled",
        0,
        Some(usage("future-accounting", 0)),
    );
    call(&ledger, "malformed", "settled", 0, Some("{}".into()));
    ledger
        .db
        .execute("INSERT INTO accounts VALUES ('bad','lifetime',-1,0)", [])
        .unwrap();
    ledger.db.execute_batch("CREATE TABLE secret_unknown (n INTEGER); WITH RECURSIVE n(v) AS (VALUES(1) UNION ALL SELECT v+1 FROM n WHERE v<10002) INSERT INTO secret_unknown SELECT v FROM n;").unwrap();
    let audit = ledger.reconciliation_audit(&authority()).unwrap();
    assert_eq!(audit.history.missing_or_invalid_usage, 2);
    assert_eq!(audit.history.unsupported_accounting_records, 1);
    assert_eq!(audit.history.invalid_accounting_rows, 1);
    assert_eq!(audit.history.other_history_rows.observed, MAX_ROWS);
    assert!(audit.history.other_history_rows.capped);
    assert!(!audit.counts_complete);
    assert!(
        audit
            .blockers
            .contains(&PersonalAuditBlocker::ObservationLimit)
    );
    assert!(
        audit
            .blockers
            .contains(&PersonalAuditBlocker::UnknownHistory)
    );
    assert!(
        !serde_json::to_string(&audit)
            .unwrap()
            .contains("secret_unknown")
    );
}

#[test]
fn personal_audit_never_initializes_unscoped_or_repairs_unsupported_schema() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("unscoped.sqlite");
    let mut ledger = Ledger::open(&path, OWNER).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        ledger.reconciliation_audit(&authority()),
        Err(Error::Unauthorized)
    ));
    assert_eq!(before, std::fs::read(&path).unwrap());
    drop(ledger);
    let (_root, path, mut ledger) = fixture();
    ledger.db.pragma_update(None, "user_version", 99).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        ledger.reconciliation_audit(&authority()),
        Err(Error::ForeignDatabase)
    ));
    assert_eq!(before, std::fs::read(&path).unwrap());
    ledger.db.pragma_update(None, "user_version", 1).unwrap();
    ledger
        .db
        .execute_batch("ALTER TABLE accounts RENAME COLUMN held TO legacy_held")
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        ledger.reconciliation_audit(&authority()),
        Err(Error::ForeignDatabase)
    ));
    assert_eq!(before, std::fs::read(path).unwrap());
}

#[test]
fn personal_existing_readonly_inspector_never_constructs_or_recovers_ledger() {
    let (_root, path, mut source) = fixture();
    bind(&mut source);
    call(&source, "pending-original", "pending", 400, None);
    assert!(matches!(
        audit_readonly_connection(&mut source.db, &authority()),
        Err(Error::Unauthorized)
    ));
    let before = snapshot(&source);
    let mut observed =
        Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let audit = audit_readonly_connection(&mut observed, &authority()).unwrap();
    assert_eq!(audit.history.pending_calls, 1);
    assert_eq!(before, snapshot(&source));
    let state: String = source
        .db
        .query_row(
            "SELECT state FROM calls WHERE id='pending-original'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "pending");
}
