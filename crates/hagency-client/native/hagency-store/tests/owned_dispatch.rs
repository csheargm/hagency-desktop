mod common;
use common::*;
use hagency_core::tasks::*;
use hagency_store::{DomainRepository, EffectOutcome, Error, OwnedFailure, OwnedObservation};
use serde_json::json;

fn setup() -> (
    tempfile::TempDir,
    DomainRepository,
    String,
    RunnerCapability,
) {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    let pool = resource("pool", "seat", 1000);
    db.put_resource(&pool).unwrap();
    let proof = proof(&request("allocated", "Worker", &pool, 100));
    let e = db.admit(&proof, 1000).unwrap();
    db.approve("approved", &proof, 1000).unwrap();
    let effect = db.claim_effect().unwrap().unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "offline_identity".into(),
        },
    )
    .unwrap();
    db.register_session(&SessionBinding {
        id: "session".into(),
        engagement_id: e.id.clone(),
        room_id: "!project:example.test".into(),
        thread_root: Some("$thread".into()),
    })
    .unwrap();
    db.register_workspace("workspace").unwrap();
    db.create_canonical_task("task", "session", "Host-bound work", 1000)
        .unwrap();
    db.enqueue_dispatch(&DispatchInput { id: "dispatch".into(), session_id: "session".into(), task_id: Some("task".into()), resources: vec![ResourceLease { id: "workspace".into(), exclusive: true }], payload: json!({"instruction":"verify 中文", "cwd":"/model/path", "task_id":"impostor", "score":0.25}) }).unwrap();
    let cap = db
        .claim_dispatch("runner", 1001, 1000, 2000, 1)
        .unwrap()
        .unwrap();
    (root, db, e.id, cap)
}
fn sql(root: &tempfile::TempDir) -> rusqlite::Connection {
    rusqlite::Connection::open(root.path().join("state/domain.sqlite3")).unwrap()
}

fn policy_snapshot_count(root: &tempfile::TempDir) -> u64 {
    sql(root)
        .query_row("SELECT COUNT(*) FROM owned_dispatch_execution_policies", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn native_owned_execution_policy_changed_before_start_requires_fresh_scope() {
    let (root, mut db, engagement, cap) = setup();
    let prepared = db.owned_dispatch_scope(&cap, 1002).unwrap();
    assert!(!prepared.execution_policy().yolo);
    db.set_execution_policy(&engagement, Some(&json!({"yolo": true})), 1003)
        .unwrap();
    assert!(matches!(
        db.start_owned_dispatch(&cap, prepared.fingerprint(), 1004),
        Err(Error::RunnerAuthority)
    ));
    assert_eq!(policy_snapshot_count(&root), 0);
    assert_eq!(db.canonical_task("task").unwrap().status, TaskState::Created);
    let current = db.owned_dispatch_scope(&cap, 1004).unwrap();
    assert!(current.execution_policy().yolo);
    assert_ne!(current.fingerprint(), prepared.fingerprint());
    let started = db.start_owned_dispatch(&cap, current.fingerprint(), 1005).unwrap();
    assert_eq!(started.fingerprint(), current.fingerprint());
    assert!(started.execution_policy().yolo);
    assert_eq!(policy_snapshot_count(&root), 1);
}

#[test]
fn native_owned_execution_policy_survives_setting_change_and_runtime_restart() {
    let (root, mut db, engagement, cap) = setup();
    db.set_execution_policy(&engagement, Some(&json!({"yolo": true})), 1002)
        .unwrap();
    let prepared = db.owned_dispatch_scope(&cap, 1002).unwrap();
    let started = db.start_owned_dispatch(&cap, prepared.fingerprint(), 1003).unwrap();
    db.set_execution_policy(&engagement, Some(&json!({"yolo": false})), 1004)
        .unwrap();
    assert!(started.execution_policy().yolo);
    db.check_owned_dispatch(&cap, started.fingerprint(), 1005).unwrap();
    // The live attempt keeps its captured policy while current owner settings
    // change. The ordinary runtime restart below must retire that old attempt.
    db.check_owned_dispatch(&cap, started.fingerprint(), 1006).unwrap();
    let retained: (String, bool, u64) = sql(&root).query_row(
        "SELECT engagement_id,yolo,captured_at FROM owned_dispatch_execution_policies WHERE dispatch_id='dispatch'",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(retained, (engagement.clone(), true, 1003));
    // A different task on the same agent takes the new owner setting.
    db.register_session(&SessionBinding {
        id: "next_session".into(), engagement_id: engagement,
        room_id: "!project:example.test".into(), thread_root: Some("$next_thread".into()),
    }).unwrap();
    db.register_workspace("next_workspace").unwrap();
    db.create_canonical_task("next_task", "next_session", "Next policy", 1006).unwrap();
    db.enqueue_dispatch(&DispatchInput {
        id: "next_dispatch".into(), session_id: "next_session".into(),
        task_id: Some("next_task".into()),
        resources: vec![ResourceLease { id: "next_workspace".into(), exclusive: true }],
        payload: json!({"instruction":"Next policy"}),
    }).unwrap();
    let next_cap = db.claim_dispatch("next_runner", 1007, 1000, 2000, 8).unwrap().unwrap();
    assert_eq!(next_cap.dispatch_id, "next_dispatch");
    let next = db.owned_dispatch_scope(&next_cap, 1008).unwrap();
    assert!(!next.execution_policy().yolo);
    db.start_owned_dispatch(&next_cap, next.fingerprint(), 1009).unwrap();
    assert_eq!(policy_snapshot_count(&root), 2);
    db.check_owned_dispatch(&cap, started.fingerprint(), 1010).unwrap();
    drop(db);
    let db = DomainRepository::open(&root.path().join("state")).unwrap();
    assert!(db.check_owned_dispatch(&cap, started.fingerprint(), 1011).is_err());
    assert_eq!(policy_snapshot_count(&root), 2);
}

#[test]
fn native_owned_execution_policy_capture_rolls_back_with_failed_start() {
    let (root, mut db, engagement, cap) = setup();
    db.set_execution_policy(&engagement, Some(&json!({"yolo": true})), 1002)
        .unwrap();
    let prepared = db.owned_dispatch_scope(&cap, 1002).unwrap();
    sql(&root).execute_batch(
        "CREATE TRIGGER reject_test_start BEFORE UPDATE OF state ON runner_dispatches \
         WHEN NEW.state='started' BEGIN SELECT RAISE(ABORT,'test start failure'); END;",
    ).unwrap();
    assert!(db.start_owned_dispatch(&cap, prepared.fingerprint(), 1003).is_err());
    assert_eq!(policy_snapshot_count(&root), 0);
    assert_eq!(db.canonical_task("task").unwrap().status, TaskState::Created);
    sql(&root).execute_batch("DROP TRIGGER reject_test_start").unwrap();
    let started = db.start_owned_dispatch(&cap, prepared.fingerprint(), 1004).unwrap();
    assert!(started.execution_policy().yolo);
    assert_eq!(policy_snapshot_count(&root), 1);
}

#[test]
fn native_owned_execution_policy_unstarted_retry_uses_current_setting() {
    let (root, mut db, engagement, old_cap) = setup();
    let old = db.owned_dispatch_scope(&old_cap, 1002).unwrap();
    assert!(!old.execution_policy().yolo);
    db.set_execution_policy(&engagement, Some(&json!({"yolo": true})), 2002).unwrap();
    let cap = db.claim_dispatch("replacement", 2003, 1000, 2000, 1).unwrap().unwrap();
    assert_eq!(cap.dispatch_id, old_cap.dispatch_id);
    assert!(cap.fence > old_cap.fence);
    assert!(db.start_owned_dispatch(&old_cap, old.fingerprint(), 2004).is_err());
    assert_eq!(policy_snapshot_count(&root), 0);
    let scope = db.owned_dispatch_scope(&cap, 2004).unwrap();
    assert!(scope.execution_policy().yolo);
    db.start_owned_dispatch(&cap, scope.fingerprint(), 2005).unwrap();
    assert_eq!(policy_snapshot_count(&root), 1);
}

#[test]
fn native_owned_stopped_inspection_store() {
    let (root, mut db, _, cap) = setup();
    let before = db.owned_dispatch_scope(&cap, 1002).unwrap();
    // Store-only contract fixture: not claimed as physical process evidence.
    let inventory =
        json!({"profile":"stopped-content-inventory-v1","root":{"fixture":true},"entries":[]});
    assert!(matches!(
        db.record_owned_stop_inspection(&cap, &before, &inventory, 1002),
        Err(Error::RunnerAuthority)
    ));
    let started = db
        .start_owned_dispatch(&cap, before.fingerprint(), 1003)
        .unwrap();
    assert!(matches!(
        db.record_owned_stop_inspection(&cap, &started, &inventory, 1004),
        Err(Error::State)
    ));
    assert_eq!(
        db.observe_owned_failure(&cap, OwnedFailure::Protocol, 4000)
            .unwrap(),
        OwnedObservation::Fenced
    );
    let digest = db
        .record_owned_stop_inspection(&cap, &started, &inventory, 4001)
        .unwrap();
    let receipt = db
        .owned_stop_inspection(&cap.dispatch_id, cap.fence)
        .unwrap()
        .unwrap();
    assert_eq!(receipt["digest"], digest);
    assert_eq!(
        db.record_owned_stop_inspection(&cap, &started, &inventory, 4002)
            .unwrap(),
        digest
    );
    assert_eq!(
        db.owned_stop_inspection(&cap.dispatch_id, cap.fence)
            .unwrap(),
        Some(receipt.clone())
    );
    let mut changed = inventory.clone();
    changed["entries"] = json!([{"path":"changed"}]);
    assert!(matches!(
        db.record_owned_stop_inspection(&cap, &started, &changed, 4003),
        Err(Error::Conflict)
    ));
    for field in ["secret", "runner", "dispatch", "fence"] {
        let mut foreign = cap.clone();
        match field {
            "secret" => foreign.secret.push('x'),
            "runner" => foreign.runner_id.push('x'),
            "dispatch" => foreign.dispatch_id.push('x'),
            _ => foreign.fence += 1,
        }
        assert!(matches!(
            db.record_owned_stop_inspection(&foreign, &started, &inventory, 4004),
            Err(Error::RunnerAuthority)
        ));
    }
    assert_eq!(
        db.canonical_task("task").unwrap().status,
        TaskState::InProgress
    );
    let counts:(u64,u64,u64,u64)=sql(&root).query_row("SELECT (SELECT COUNT(*) FROM resource_leases),(SELECT COUNT(*) FROM dispatch_stops WHERE settled_at IS NULL),(SELECT dirty FROM workspace_resources),(SELECT quarantined FROM runner_sessions)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(counts, (1, 1, 1, 1));
    drop(db);
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    assert_eq!(
        db.record_owned_stop_inspection(&cap, &started, &inventory, 5000)
            .unwrap(),
        digest
    );
    assert_eq!(
        db.owned_stop_inspection(&cap.dispatch_id, cap.fence)
            .unwrap(),
        Some(receipt)
    );
}

#[tokio::test]
async fn native_fleet_runner_service_scope() {
    let (root, mut db, first, cap) = setup();
    let pool = resource("pool", "seat", 1000);
    let proof = proof(&request("second_allocated", "OtherWorker", &pool, 100));
    let second = db.admit(&proof, 1000).unwrap();
    db.approve("second_approved", &proof, 1000).unwrap();
    let effect = db.claim_effect().unwrap().unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "offline second routing fixture".into(),
        },
    )
    .unwrap();
    db.register_session(&SessionBinding {
        id: "second_session".into(),
        engagement_id: second.id.clone(),
        room_id: "!other:example.test".into(),
        thread_root: None,
    })
    .unwrap();
    db.register_workspace("second_workspace").unwrap();
    db.create_canonical_task("second_task", "second_session", "Other original task", 1000)
        .unwrap();
    db.enqueue_dispatch(&DispatchInput {
        id: "second_dispatch".into(),
        session_id: "second_session".into(),
        task_id: Some("second_task".into()),
        resources: vec![ResourceLease {
            id: "second_workspace".into(),
            exclusive: true,
        }],
        payload: json!({"instruction":"original backend selection"}),
    })
    .unwrap();
    let other = db
        .claim_dispatch("other_runner", 1002, 1000, 2000, 2)
        .unwrap()
        .unwrap();
    assert_eq!(db.runner_service_engagement(&cap).unwrap(), first);
    assert_eq!(db.runner_service_engagement(&other).unwrap(), second.id);
    let before: String=sql(&root).query_row("SELECT json_group_array(json_array(id,state,fence,runner_id,lease_until,capability_until)) FROM runner_dispatches",[],|row|row.get(0)).unwrap();
    for field in ["secret", "runner", "dispatch", "fence"] {
        let mut foreign = cap.clone();
        match field {
            "secret" => foreign.secret = other.secret.clone(),
            "runner" => foreign.runner_id = other.runner_id.clone(),
            "dispatch" => foreign.dispatch_id = other.dispatch_id.clone(),
            _ => foreign.fence += 1,
        }
        assert!(matches!(
            db.runner_service_engagement(&foreign),
            Err(Error::RunnerAuthority)
        ));
    }
    // Expired credentials still name the historical backend, but do not
    // resurrect a current scope, extend leases, or mutate either attempt.
    assert!(db.owned_dispatch_scope(&cap, 4000).is_err());
    assert_eq!(db.runner_service_engagement(&cap).unwrap(), first);
    let after: String=sql(&root).query_row("SELECT json_group_array(json_array(id,state,fence,runner_id,lease_until,capability_until)) FROM runner_dispatches",[],|row|row.get(0)).unwrap();
    assert_eq!(before, after);
    let store = hagency_store::DomainStore::start(db, 8).unwrap();
    assert_eq!(store.runner_service_engagement(cap).await.unwrap(), first);
    assert_eq!(
        store.runner_service_engagement(other).await.unwrap(),
        second.id
    );
    store.shutdown().await.unwrap();
}

#[test]
fn native_owned_dispatch_start_scope() {
    let (root, mut db, _, cap) = setup();
    let before = db.owned_dispatch_scope(&cap, 1002).unwrap();
    assert_eq!(before.task().id, "task");
    assert_eq!(before.task().status, TaskState::Created);
    assert_eq!(before.input().resources[0].id, "workspace");
    assert_eq!(before.input().payload["cwd"], "/model/path"); // Data only.
    let mut edited = resource("pool", "seat", 1000);
    edited.model = "gpt-5.5".into();
    assert!(matches!(db.edit_resource(&edited, None), Err(Error::State)));
    // Deliberate stored-current-profile drift: the completed provision effect
    // remains the frozen execution profile, not this mutable catalog record.
    sql(&root)
        .execute(
            "UPDATE resources SET config=?1",
            [serde_json::to_string(&edited).unwrap()],
        )
        .unwrap();
    assert_eq!(
        db.owned_dispatch_scope(&cap, 1002)
            .unwrap()
            .resource()
            .model,
        "gpt-5.6-sol"
    );
    assert!(matches!(
        db.start_owned_dispatch(&cap, "wrong", 1003),
        Err(Error::RunnerAuthority)
    ));
    assert_eq!(
        db.canonical_task("task").unwrap().status,
        TaskState::Created
    );
    let fingerprint = before.fingerprint().to_owned();
    let started = db.start_owned_dispatch(&cap, &fingerprint, 1003).unwrap();
    assert_eq!(started.fingerprint(), fingerprint);
    assert_eq!(started.task().status, TaskState::InProgress);
    assert!(db.start_owned_dispatch(&cap, &fingerprint, 1004).is_err());
    assert!(db.check_owned_dispatch(&cap, "other_scope", 1004).is_err());
    assert_eq!(
        db.check_owned_dispatch(&cap, &fingerprint, 1004)
            .unwrap()
            .status,
        TaskState::InProgress
    );
    sql(&root)
        .execute(
            "UPDATE workspace_resources SET dirty=1 WHERE id='workspace'",
            [],
        )
        .unwrap();
    assert!(matches!(
        db.check_owned_dispatch(&cap, &fingerprint, 1005),
        Err(Error::Quarantined)
    ));
    assert_eq!(
        db.observe_owned_failure(&cap, OwnedFailure::CleanupUnknown, 1006)
            .unwrap(),
        OwnedObservation::Fenced
    );
    let held: u64 = sql(&root)
        .query_row("SELECT COUNT(*) FROM resource_leases", [], |r| r.get(0))
        .unwrap();
    assert_eq!(held, 1);
}

#[test]
fn native_owned_dispatch_negative_historical_fence() {
    let (root, mut db, engagement, cap) = setup();
    let digest = db
        .owned_dispatch_scope(&cap, 1002)
        .unwrap()
        .fingerprint()
        .to_owned();
    db.start_owned_dispatch(&cap, &digest, 1003).unwrap();
    db.revoke("revoke", &engagement).unwrap();
    assert_eq!(
        db.observe_owned_failure(&cap, OwnedFailure::LostAuthority, 4000)
            .unwrap(),
        OwnedObservation::Fenced
    );
    assert_eq!(
        db.canonical_task("task").unwrap().status,
        TaskState::InProgress
    );
    assert_eq!(
        sql(&root)
            .query_row("SELECT COUNT(*) FROM resource_leases", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
    let (root, mut db, _, old) = setup();
    db.reconcile_dispatches(2500).unwrap(); // Unstarted attempt expires/requeues.
    let replacement = db
        .claim_dispatch("replacement", 2501, 1000, 2000, 1)
        .unwrap()
        .unwrap();
    assert!(replacement.fence > old.fence);
    assert_eq!(
        db.observe_owned_failure(&old, OwnedFailure::StartUnknown, 2502)
            .unwrap(),
        OwnedObservation::Historical
    );
    let current = db.owned_dispatch_scope(&replacement, 2503).unwrap();
    db.start_owned_dispatch(&replacement, current.fingerprint(), 2503)
        .unwrap();
    assert_eq!(
        sql(&root)
            .query_row("SELECT COUNT(*) FROM resource_leases", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        sql(&root)
            .query_row(
                "SELECT dirty FROM workspace_resources WHERE id='workspace'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    let forged = RunnerCapability {
        secret: "0".repeat(64),
        ..old
    };
    assert!(matches!(
        db.observe_owned_failure(&forged, OwnedFailure::Cancelled, 2504),
        Err(Error::RunnerAuthority)
    ));
}

/// Board #111: a run the host settled as `outcome_unknown` by RESTART — not by
/// a reported failure — must still be listed for the operator. The quarantine
/// notice a session then shows ("a previous runner in this session stopped
/// after work may have started") is gated on `quarantined=1` plus an
/// `outcome_unknown` dispatch (`task_intents::quarantined_waiting_notice`); the
/// console's review page must cover the SAME set, or the operator has no way out.
#[tokio::test]
async fn native_console_review_lists_a_restart_settled_outcome_unknown() {
    let (root, mut db, engagement, cap) = setup();
    let before = db.owned_dispatch_scope(&cap, 1002).unwrap();
    db.start_owned_dispatch(&cap, before.fingerprint(), 1003)
        .unwrap();
    // The host dies mid-turn. Reopening settles the started run as unknown
    // (`recover_all` -> `lose(.., "restart")`), which also quarantines it.
    drop(db);
    let db = DomainRepository::open(&root.path().join("state")).unwrap();
    let raw = sql(&root);
    let (state, quarantined, stops): (String, i64, i64) = raw
        .query_row(
            "SELECT d.state,s.quarantined,(SELECT COUNT(*) FROM dispatch_stops WHERE dispatch_id=d.id) \
             FROM runner_dispatches d JOIN runner_sessions s ON s.id=d.session_id WHERE d.id=?1",
            [&cap.dispatch_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, "outcome_unknown", "the restart settle");
    assert_eq!(quarantined, 1, "the session refuses the next turn");
    // The operator's only route out. The quarantine is about this dispatch, so
    // the review page must show it.
    let listed = db.stopped_dispatches_for_agent(&engagement, "").unwrap();
    assert_eq!(
        listed["dispatches"].as_array().unwrap().len(),
        1,
        "the review page must list the dispatch the quarantine is about (stop rows: {stops}): {listed}"
    );
}

/// Board #111, the whole operator path for a RESTART-settled dispatch:
/// review lists it -> inspect -> resolve -> the session accepts a turn again.
/// The board's acceptance is exactly this chain, so it is driven here rather
/// than asserting the listing alone.
#[tokio::test]
async fn native_console_review_resolves_a_restart_settled_outcome_unknown() {
    let (root, mut db, engagement, cap) = setup();
    let before = db.owned_dispatch_scope(&cap, 1002).unwrap();
    db.start_owned_dispatch(&cap, before.fingerprint(), 1003)
        .unwrap();
    drop(db);
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();

    // 1. The review page lists the dispatch the quarantine is about.
    let listed = db.stopped_dispatches_for_agent(&engagement, "").unwrap();
    let rows = listed["dispatches"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "the review page lists it: {listed}");
    assert_eq!(rows[0]["dispatchId"], cap.dispatch_id);

    // 2. Inspect it.
    let inspection = db
        .begin_outcome_inspection(&engagement, &cap.dispatch_id, 60_000, 10_000)
        .expect("the operator must be able to inspect the quarantined dispatch");
    let command = hagency_store::OutcomeResolution {
        original: cap.dispatch_id.clone(),
        request_id: "operator_resolution".into(),
        inspection_id: inspection["inspectionId"].as_str().unwrap().into(),
        inspection_token: inspection["inspectionToken"].as_str().unwrap().into(),
        action: hagency_store::OutcomeAction::AcceptCompleted,
        operator_note: "Reviewed the retained workspace".into(),
        replacement: None,
    };

    // 3. Resolve it.
    db.resolve_stopped_dispatch(&engagement, &command, 10_001)
        .expect("the operator must be able to settle the quarantined dispatch");

    // 4. The session accepts a turn again — the point of the whole page.
    let raw = sql(&root);
    let (quarantined, dirty, unresolved): (i64, i64, i64) = raw
        .query_row(
            "SELECT s.quarantined,w.dirty,(SELECT COUNT(*) FROM unresolved_dispatches u WHERE u.id=?1) \
             FROM runner_sessions s JOIN workspace_resources w ON w.id='workspace' WHERE s.id='session'",
            [&cap.dispatch_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        quarantined, 0,
        "the session no longer refuses the next turn"
    );
    assert_eq!(dirty, 0, "the workspace is no longer held");
    assert_eq!(unresolved, 0, "nothing unsettled remains");
}
