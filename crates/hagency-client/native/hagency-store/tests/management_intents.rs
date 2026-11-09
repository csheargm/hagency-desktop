use hagency_store::{Error, management::*};
use serde_json::json;
use std::time::{Duration, Instant};

fn context(account: &str, epoch: u64, generation: u64) -> HostContext {
    HostContext::new(
        account.into(),
        epoch,
        "a".repeat(64),
        epoch,
        "executor".into(),
        generation,
    )
    .unwrap()
}
fn session(store: &ServerManagementJournal, ctx: &HostContext, permissions: &[Permission]) -> Session {
    Session::new_server(
        store.clone(),
        ctx.clone(),
        permissions.iter().copied(),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
const GRANTS: &[Permission] = &[
    Permission::EnrollResources,
    Permission::ReadIntents,
    Permission::AcknowledgeIntents,
];
fn intent(id: &str, tokens: u64) -> Intent {
    Intent::new(
        IntentOperation::EnrollResource,
        id.into(),
        &json!({"commandId":id,"runtime":"codex","modelId":"gpt-5.6-sol","monthlyTokens":tokens}),
    )
    .unwrap()
}

#[tokio::test]
async fn custody_survives_restart_and_requires_known_reply_before_acknowledgement() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("ledger");
    let store = ServerManagementJournal::open(&state, &"a".repeat(64), "@owner:test").unwrap();
    let ctx = context("@owner:test", 1, 1);
    let host = session(&store, &ctx, GRANTS);
    let first = host
        .prepare_intent(&ctx, intent("stable", 5000))
        .await
        .unwrap();
    assert!(first.reply.is_none());
    assert!(first.is_first_attempt());
    assert!(
        !host
            .prepare_intent(&ctx, intent("stable", 5000))
            .await
            .unwrap()
            .is_first_attempt()
    );
    assert!(
        matches!(
            host.acknowledge_intent(&ctx, "stable".into()).await,
            Err(Error::Conflict)
        ),
        "an unknown outcome cannot be forgotten"
    );
    assert!(matches!(
        host.prepare_intent(&ctx, intent("stable", 9000)).await,
        Err(Error::Conflict)
    ));
    host.retire();
    store.shutdown().await.unwrap();
    drop(host);
    drop(store);

    let store = ServerManagementJournal::open(&state, &"a".repeat(64), "@owner:test").unwrap();
    let ctx = context("@owner:test", 2, 2);
    let host = session(&store, &ctx, GRANTS);
    let recovered = host.pending_intents(&ctx).await.unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].command_id, "stable");
    assert_eq!(recovered[0].input["monthlyTokens"], 5000);
    assert!(recovered[0].reply.is_none());
    assert!(!recovered[0].is_first_attempt());
    let reply = IntentReply::Success(json!({"resourceId":"original_resource"}));
    host.record_intent_reply(&ctx, intent("stable", 5000), reply.clone())
        .await
        .unwrap();
    assert_eq!(
        host.prepare_intent(&ctx, intent("stable", 5000))
            .await
            .unwrap()
            .reply,
        Some(reply)
    );
    assert!(
        host.record_intent_reply(
            &ctx,
            intent("stable", 5000),
            IntentReply::Failure {
                status: 409,
                code: "different_outcome".into()
            }
        )
        .await
        .is_err()
    );
    host.acknowledge_intent(&ctx, "stable".into())
        .await
        .unwrap();
    host.acknowledge_intent(&ctx, "stable".into())
        .await
        .unwrap();
    assert!(host.pending_intents(&ctx).await.unwrap().is_empty());
    assert!(
        host.prepare_intent(&ctx, intent("stable", 5000))
            .await
            .is_err(),
        "acknowledged IDs remain tombstones"
    );
    let db = rusqlite::Connection::open(state.join("server-management.sqlite3")).unwrap();
    let row: (Option<String>, Option<String>, String) = db
        .query_row(
            "SELECT input,reply,state FROM management_intents",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, (None, None, "acknowledged".into()));
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn custody_recovery_requires_current_account_device_generation_and_operation_grant() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let store = ServerManagementJournal::open(&root.path().join("ledger"), &"a".repeat(64), "@owner:test")
    .unwrap();
    let ctx = context("@owner:test", 1, 1);
    let host = session(&store, &ctx, GRANTS);
    host.prepare_intent(&ctx, intent("scoped", 1))
        .await
        .unwrap();
    let other_device = HostContext::new(
        "@owner:test".into(),
        1,
        "a".repeat(64),
        1,
        "another-executor".into(),
        1,
    )
    .unwrap();
    assert!(matches!(Session::new_server(store.clone(),context("@other:test",1,1),GRANTS.iter().copied(),Instant::now()+Duration::from_secs(60)),Err(Error::LocalAuthority)));
    assert!(session(&store, &other_device, GRANTS).pending_intents(&other_device).await.unwrap().is_empty());
    assert!(host.pending_intents(&other_device).await.is_err());
    let renewed = context("@owner:test", 2, 2);
    assert!(
        host.pending_intents(&renewed).await.is_err(),
        "old sessions cannot adopt a newer generation"
    );
    assert_eq!(
        session(&store, &renewed, GRANTS)
            .pending_intents(&renewed)
            .await
            .unwrap()
            .len(),
        1,
        "freshly authorized same installation can recover its original request"
    );
    let reader = session(&store, &ctx, &[Permission::ReadIntents]);
    assert!(
        reader.pending_intents(&ctx).await.unwrap().is_empty(),
        "recovery does not imply the original operation permission"
    );
    assert!(
        reader
            .prepare_intent(&ctx, intent("denied", 1))
            .await
            .is_err()
    );
    host.retire();
    assert!(host.pending_intents(&ctx).await.is_err());
    assert!(
        host.record_intent_reply(&ctx, intent("scoped", 1), IntentReply::Success(json!({})))
            .await
            .is_err()
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn custody_is_bounded_and_failed_native_persistence_leaves_no_prepared_command() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("ledger");
    let store = ServerManagementJournal::open(&state, &"a".repeat(64), "@owner:test").unwrap();
    let ctx = context("@owner:test", 1, 1);
    let host = session(&store, &ctx, GRANTS);
    let db = rusqlite::Connection::open(state.join("server-management.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_intent BEFORE INSERT ON management_intents BEGIN SELECT RAISE(ABORT,'test storage failure'); END;").unwrap();
    assert!(
        host.prepare_intent(&ctx, intent("blocked", 1))
            .await
            .is_err()
    );
    assert!(host.pending_intents(&ctx).await.unwrap().is_empty());
    db.execute_batch("DROP TRIGGER deny_intent").unwrap();
    for n in 0..8 {
        host.prepare_intent(&ctx, intent(&format!("pending_{n}"), 1))
            .await
            .unwrap();
    }
    assert!(matches!(
        host.prepare_intent(&ctx, intent("overflow", 1)).await,
        Err(Error::Capacity)
    ));
    assert_eq!(host.pending_intents(&ctx).await.unwrap().len(), 8);
    assert!(
        Intent::new(
            IntentOperation::EnrollResource,
            "large".into(),
            &json!({"commandId":"large","data":"x".repeat(16*1024)})
        )
        .is_err()
    );
    assert!(
        Intent::new(
            IntentOperation::EnrollResource,
            "different".into(),
            &json!({"commandId":"original"})
        )
        .is_err()
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn project_journal_restarts_without_promoting_resource_or_read_grants() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("ledger");
    let store = ServerManagementJournal::open(&state, &"a".repeat(64), "@manager:test").unwrap();
    let ctx = context("@manager:test", 1, 1);
    let operations = [
        (
            IntentOperation::ProjectRequest,
            Permission::RequestProjects,
            "request",
        ),
        (
            IntentOperation::ProjectDecide,
            Permission::DecideProjects,
            "decision",
        ),
        (
            IntentOperation::ProjectRevoke,
            Permission::RevokeProjects,
            "revocation",
        ),
    ];
    let make = |op, id: &str| {
        Intent::new(
            op,
            id.into(),
            &json!({"commandId":id,"projectId":"project_1"}),
        )
        .unwrap()
    };
    let reader = session(
        &store,
        &ctx,
        &[
            Permission::ReadIntents,
            Permission::ContributeResources,
            Permission::DecideInbox,
            Permission::RevokeConnections,
            Permission::ReadProjects,
        ],
    );
    for (op, permission, id) in operations {
        assert!(reader.prepare_intent(&ctx, make(op, id)).await.is_err());
        let writer = session(&store, &ctx, &[permission, Permission::ReadIntents]);
        assert!(
            writer
                .prepare_intent(&ctx, make(op, id))
                .await
                .unwrap()
                .is_first_attempt()
        );
        writer.retire();
    }
    assert!(reader.pending_intents(&ctx).await.unwrap().is_empty());
    reader.retire();
    store.shutdown().await.unwrap();
    drop(reader);
    drop(store);

    let store = ServerManagementJournal::open(&state, &"a".repeat(64), "@manager:test").unwrap();
    let renewed = context("@manager:test", 2, 2);
    for (op, permission, id) in operations {
        let writer = session(
            &store,
            &renewed,
            &[
                permission,
                Permission::ReadIntents,
                Permission::AcknowledgeIntents,
            ],
        );
        let rows = writer.pending_intents(&renewed).await.unwrap();
        assert_eq!(
            rows.len(),
            1,
            "an exact operation grant exposes only its own journal"
        );
        assert_eq!(rows[0].command_id, id);
        assert!(
            !writer
                .prepare_intent(&renewed, make(op, id))
                .await
                .unwrap()
                .is_first_attempt()
        );
        assert!(
            writer
                .acknowledge_intent(&renewed, id.into())
                .await
                .is_err(),
            "unknown results cannot be discarded"
        );
        let reply = IntentReply::Success(
            json!({"project":{"id":"project_1","allocationStatus":"awaiting_allocation"}}),
        );
        writer
            .record_intent_reply(&renewed, make(op, id), reply.clone())
            .await
            .unwrap();
        assert_eq!(
            writer
                .prepare_intent(&renewed, make(op, id))
                .await
                .unwrap()
                .reply,
            Some(reply)
        );
        writer
            .acknowledge_intent(&renewed, id.into())
            .await
            .unwrap();
        assert!(writer.pending_intents(&renewed).await.unwrap().is_empty());
    }
    store.shutdown().await.unwrap();
}

const PUBLISH: &[Permission] = &[
    Permission::PublishGrants,
    Permission::EnrollResources,
    Permission::ReadIntents,
    Permission::AcknowledgeIntents,
];
fn publication(id: &str, grant: &str, successor: bool) -> Intent {
    Intent::new(
        if successor {
            IntentOperation::PublishGrantSuccessor
        } else {
            IntentOperation::PublishGrant
        },
        id.into(),
        &json!({"commandId":id,"expectedContributionRevision":1,"receipt":{"grantId":grant}}),
    )
    .unwrap()
    .require_unused_grant_publication()
}
async fn publish(host: &Session, ctx: &HostContext, id: &str, grant: &str, successor: bool) {
    let saved = host
        .prepare_intent(ctx, publication(id, grant, successor))
        .await
        .unwrap();
    assert!(saved.is_first_attempt());
    host.record_intent_reply(
        ctx,
        publication(id, grant, successor),
        IntentReply::Success(json!({"grant":{"id":grant}})),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn replied_publications_are_history_and_never_block_other_commands() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("ledger");
    let store = ServerManagementJournal::open(&state, &"a".repeat(64), "@owner:test").unwrap();
    let ctx = context("@owner:test", 1, 1);
    let host = session(&store, &ctx, PUBLISH);
    // Twenty monthly root publications and twenty successors on one device.
    for n in 0..20 {
        publish(&host, &ctx, &format!("root_{n}"), &format!("grant_root_{n}"), false).await;
        publish(&host, &ctx, &format!("next_{n}"), &format!("grant_next_{n}"), true).await;
    }
    // Kept history is not outstanding work and is not in the pending read...
    assert!(host.pending_intents(&ctx).await.unwrap().is_empty());
    // ...but every publication is still found by its own command ID.
    for id in ["root_0", "root_19", "next_0", "next_19"] {
        let saved = host.intent(&ctx, id.into()).await.unwrap().unwrap();
        assert_eq!(saved.command_id, id);
        assert!(matches!(saved.reply, Some(IntentReply::Success(_))));
        assert!(!saved.is_first_attempt());
    }
    assert!(host.intent(&ctx, "unknown".into()).await.unwrap().is_none());
    // A grant is never published twice, however old its publication is.
    for (grant, successor) in [("grant_root_0", false), ("grant_next_0", true), ("grant_root_0", true), ("grant_next_0", false)] {
        assert!(matches!(
            host.prepare_intent(&ctx, publication("again", grant, successor)).await,
            Err(Error::Conflict)
        ));
    }
    // Other management commands still get the full outstanding bound of eight,
    // replied or not, until acknowledged; the ninth is refused.
    for n in 0..8 {
        host.prepare_intent(&ctx, intent(&format!("enroll_{n}"), 1))
            .await
            .unwrap();
    }
    for n in 0..4 {
        host.record_intent_reply(
            &ctx,
            intent(&format!("enroll_{n}"), 1),
            IntentReply::Success(json!({})),
        )
        .await
        .unwrap();
    }
    assert_eq!(host.pending_intents(&ctx).await.unwrap().len(), 8);
    assert!(matches!(
        host.prepare_intent(&ctx, intent("overflow", 1)).await,
        Err(Error::Capacity)
    ));
    // A new publication is outstanding work too while the bound is full.
    assert!(matches!(
        host.prepare_intent(&ctx, publication("root_20", "grant_root_20", false)).await,
        Err(Error::Capacity)
    ));
    host.acknowledge_intent(&ctx, "enroll_0".into()).await.unwrap();
    publish(&host, &ctx, "root_20", "grant_root_20", false).await;
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_unanswered_publication_is_outstanding_and_recovered_by_its_id() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("ledger");
    let store = ServerManagementJournal::open(&state, &"a".repeat(64), "@owner:test").unwrap();
    let ctx = context("@owner:test", 1, 1);
    let host = session(&store, &ctx, PUBLISH);
    for successor in [false, true] {
        let id = if successor { "next_pending" } else { "root_pending" };
        let grant = if successor { "grant_next" } else { "grant_root" };
        let first = host
            .prepare_intent(&ctx, publication(id, grant, successor))
            .await
            .unwrap();
        assert!(first.is_first_attempt());
        // Pending: listed, found by ID, and replayed with its original identity.
        assert!(host.pending_intents(&ctx).await.unwrap().iter().any(|i| i.command_id == id));
        let found = host.intent(&ctx, id.into()).await.unwrap().unwrap();
        assert!(found.reply.is_none());
        assert_eq!(found.input["receipt"]["grantId"], grant);
        let replay = host
            .prepare_intent(&ctx, publication(id, grant, successor))
            .await
            .unwrap();
        assert!(!replay.is_first_attempt() && replay.reply.is_none());
        // Same ID, different payload: refused.
        assert!(matches!(
            host.prepare_intent(&ctx, publication(id, "grant_other", successor)).await,
            Err(Error::Conflict)
        ));
        // It cannot be forgotten while unanswered.
        assert!(matches!(
            host.acknowledge_intent(&ctx, id.into()).await,
            Err(Error::Conflict)
        ));
    }
    // Two unanswered publications plus six other commands fill the bound.
    for n in 0..6 {
        host.prepare_intent(&ctx, intent(&format!("enroll_{n}"), 1))
            .await
            .unwrap();
    }
    assert!(matches!(
        host.prepare_intent(&ctx, intent("overflow", 1)).await,
        Err(Error::Capacity)
    ));
    // An audited rejection is recorded and still found by its ID afterwards.
    let rejection = IntentReply::Failure { status: 409, code: "resource_grant_already_exists".into() };
    host.record_intent_reply(&ctx, publication("root_pending", "grant_root", false), rejection.clone())
        .await
        .unwrap();
    assert!(!host.pending_intents(&ctx).await.unwrap().iter().any(|i| i.command_id == "root_pending"));
    assert_eq!(
        host.intent(&ctx, "root_pending".into()).await.unwrap().unwrap().reply,
        Some(rejection.clone())
    );
    assert_eq!(
        host.prepare_intent(&ctx, publication("root_pending", "grant_root", false))
            .await
            .unwrap()
            .reply,
        Some(rejection)
    );
    // Answering a publication frees exactly its slot.
    host.prepare_intent(&ctx, intent("enroll_6", 1)).await.unwrap();
    // Without the operation's permission the lookup sees nothing, as the read.
    let reader = session(&store, &ctx, &[Permission::ReadIntents]);
    assert!(reader.intent(&ctx, "next_pending".into()).await.unwrap().is_none());
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn publication_history_stays_complete_and_bounded_by_the_grant_ledger() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("ledger");
    let store = ServerManagementJournal::open(&state, &"a".repeat(64), "@owner:test").unwrap();
    let ctx = context("@owner:test", 1, 1);
    let host = session(&store, &ctx, PUBLISH);
    publish(&host, &ctx, "root_0", "grant_root_0", false).await;
    // History rows as the journal writes them, up to the 2048-grant ledger.
    let db = rusqlite::Connection::open(state.join("server-management.sqlite3")).unwrap();
    let actor: String = db
        .query_row("SELECT actor_scope FROM management_intents WHERE command_id='root_0'", [], |r| r.get(0))
        .unwrap();
    let tx = db.unchecked_transaction().unwrap();
    for n in 1..2048 {
        tx.execute(
            "INSERT INTO management_intents(actor_scope,command_id,operation,digest,input,reply,state) VALUES(?1,?2,'\"publish_grant\"',?3,?4,'{\"kind\":\"success\",\"data\":{}}','replied')",
            rusqlite::params![actor, format!("root_{n}"), "0".repeat(64), json!({"commandId":format!("root_{n}"),"receipt":{"grantId":format!("grant_root_{n}")}}).to_string()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    // With 2048 publications kept, a grant ID among them is still refused and
    // one more publication fits; once the scan bound is reached it refuses.
    assert!(matches!(
        host.prepare_intent(&ctx, publication("dup", "grant_root_2000", false)).await,
        Err(Error::Conflict)
    ));
    publish(&host, &ctx, "root_2048", "grant_root_2048", false).await;
    assert!(matches!(
        host.prepare_intent(&ctx, publication("root_2049", "grant_root_2049", false)).await,
        Err(Error::Capacity)
    ));
    assert!(matches!(
        host.require_unused_grant_publication(&ctx, "grant_root_2049".into()).await,
        Err(Error::Capacity)
    ));
    // Neither the history nor the bound blocks the pending read or other work.
    assert!(host.pending_intents(&ctx).await.unwrap().is_empty());
    host.prepare_intent(&ctx, intent("enroll", 1)).await.unwrap();
    store.shutdown().await.unwrap();

    // History must stay complete: an acknowledged (scrubbed) publication makes
    // every later publication fail closed, root or successor.
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("ledger");
    let store = ServerManagementJournal::open(&state, &"a".repeat(64), "@owner:test").unwrap();
    let host = session(&store, &ctx, PUBLISH);
    publish(&host, &ctx, "root_0", "grant_root_0", false).await;
    publish(&host, &ctx, "next_0", "grant_next_0", true).await;
    host.acknowledge_intent(&ctx, "next_0".into()).await.unwrap();
    assert!(host.intent(&ctx, "next_0".into()).await.unwrap().is_none());
    for successor in [false, true] {
        assert!(matches!(
            host.prepare_intent(&ctx, publication("later", "grant_later", successor)).await,
            Err(Error::Conflict)
        ));
    }
    assert!(matches!(
        host.require_unused_grant_publication(&ctx, "grant_later".into()).await,
        Err(Error::Conflict)
    ));
    store.shutdown().await.unwrap();
}
