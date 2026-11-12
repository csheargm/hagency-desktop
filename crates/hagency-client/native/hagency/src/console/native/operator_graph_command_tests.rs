use super::*;
use crate::console::native::reconciliation_sources::tests::Fixture;
use crate::console::native::{
    ManagementPackage, OperatorGraphDetailQuery, OperatorGraphNodeQuery,
};
use hagency_store::owner_resources::{OwnerResourceRegistry};
use serde_json::json;
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
#[path = "operator_graph_recipients/integration_tests.rs"]
mod recipient_tests;

fn package(instance: u64) -> ManagementPackage {
    ManagementPackage {
        digest: "a".repeat(64),
        instance,
        account_epoch: 1,
        permissions: vec![
            Permission::ReadResources,
            Permission::PrepareSourceQuiescence,
            Permission::ReadOperatorGraphs,
            Permission::CreateOperatorGraphs,
            Permission::UpdateOperatorGraphNodes,
            Permission::CancelOperatorGraphs,
            Permission::ReadIntents,
            Permission::AcknowledgeIntents,
        ],
    }
}
async fn adopted() -> (Fixture, OwnerResourceRegistry) {
    let f = Fixture::new().await;
    let registry=OwnerResourceRegistry::isolated_for_tests(&f.temp.path().join("canonical")).unwrap();
    let f=f.initialize_current(registry.clone()).await;
    (f, registry)
}
fn create_input() -> OperatorGraphCreate {
    let mut nodes = serde_json::Map::new();
    nodes.insert(
        "root".into(),
        json!({"assignee":"Recorded reviewer","description":"Inspect".repeat(300)}),
    );
    for n in 0..19 {
        nodes.insert(format!("node_{n:02}"),json!({"assignee":"Recorded worker","description":"Apply".repeat(200),"dependsOn":["root"],"conditionJson":serde_json::to_string(&json!({"dep":"root","path":"score","eq":if n==18 {0.5} else {0.75}})).unwrap()}));
    }
    serde_json::from_value(json!({"label":"Original reviewed graph","nodes":nodes})).unwrap()
}
fn counts(f: &Fixture) -> (u64, u64, u64, u64) {
    let db = rusqlite::Connection::open_with_flags(
        f.temp.path().join("canonical/domain.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.query_row("SELECT (SELECT COUNT(*) FROM owner_operator_graphs),(SELECT COUNT(*) FROM owner_operator_graph_outbox),(SELECT COUNT(*) FROM owner_operator_graph_outbox WHERE state='pending_binding'),(SELECT COUNT(*) FROM owner_managed_calls)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap()
}
#[test]
fn native_graph_workflows_restart_review_cas_and_no_runtime_dispatch() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "console::native::operator_graph_commands::tests::native_graph_workflows_process_child",
            "--exact",
            "--nocapture",
        ])
        .env("HAGENCY_GRAPH_WORKFLOW_FIXTURE", "1")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
#[tokio::test]
async fn native_graph_workflows_process_child() {
    if std::env::var("HAGENCY_GRAPH_WORKFLOW_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let (f, registry) = adopted().await;
    let manager = f.owner.management(package(2)).await.unwrap();
    let prepared = manager
        .prepare_operator_graph_create(create_input())
        .await
        .unwrap();
    assert!(
        prepared.command_id.starts_with("graphcmd_") && prepared.graph_id.starts_with("graph_")
    );
    assert_eq!(prepared.expected_revision, 0);
    assert_eq!(prepared.delivery_state, "pending_binding");
    assert!(!prepared.runtime_delivery_authorized);
    assert_eq!(
        counts(&f),
        (0, 0, 0, 0),
        "preparation is private custody only"
    );
    assert!(
        manager
            .operator_graph_page(Default::default())
            .await
            .unwrap()
            .graphs
            .is_empty()
    );
    let reviewed = manager
        .review_saved_operator_graph(prepared.command_id.clone())
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&reviewed.change).unwrap(),
        serde_json::to_value(&prepared.change).unwrap()
    );
    assert!(
        serde_json::to_vec(&reviewed.change).unwrap().len() > 16 * 1024,
        "exercise original-intent limit regression"
    );
    let saved = manager.saved_commands().await.unwrap();
    let original: OperatorGraphCommand = serde_json::from_value(saved[0].input.clone()).unwrap();
    // Real canonical commit with intentionally lost native intent reply. A new
    // owner and package must recover this exact receipt rather than create again.
    let (context, session) = manager.authorized().await.unwrap();
    let (native_registry, profile) = session
        .operator_graph_access(&context, Permission::CreateOperatorGraphs)
        .unwrap();
    let journal =
        OperatorGraphJournal::new(native_registry, profile, OPERATOR_GRAPH_SCOPE.into()).unwrap();
    let committed = journal
        .apply(
            original.command_id.clone(),
            original.graph_id.clone(),
            original.expected_revision,
            original.change.clone(),
        )
        .await
        .unwrap();
    assert_eq!(counts(&f), (1, 1, 1, 0));
    assert!(
        manager
            .review_saved_operator_graph(prepared.command_id.clone())
            .await
            .unwrap()
            .result
            .is_none()
    );
    manager.retire();
    assert!(
        manager
            .apply_saved_operator_graph(prepared.command_id.clone())
            .await
            .is_err()
    );
    drop(journal);
    drop(session);
    drop(manager);
    let f = f
        .restart_current(registry.clone(), "current_fixture".into())
        .await;
    let manager = f.owner.management(package(3)).await.unwrap();
    let first = manager
        .apply_saved_operator_graph(prepared.command_id.clone())
        .await
        .unwrap();
    assert_eq!(first, result(committed, &original.change));
    assert_eq!(
        manager
            .apply_saved_operator_graph(prepared.command_id.clone())
            .await
            .unwrap(),
        first
    );
    assert_eq!(counts(&f), (1, 1, 1, 0));
    let page = manager
        .operator_graph_page(Default::default())
        .await
        .unwrap();
    assert_eq!(page.graphs.len(), 1);
    assert_eq!(page.graphs[0].owner, "@owner:test");
    assert_eq!(page.graphs[0].node_count, 20);
    let details = manager
        .operator_graph_detail(OperatorGraphDetailQuery {
            id: prepared.graph_id.clone(),
            node_after: None,
        })
        .await
        .unwrap();
    assert_eq!(details.nodes.len(), 16);
    let tail = manager
        .operator_graph_detail(OperatorGraphDetailQuery {
            id: prepared.graph_id.clone(),
            node_after: details.next_after.clone(),
        })
        .await
        .unwrap();
    assert_eq!(tail.nodes.len(), 4);
    assert!(tail.next_after.is_none());
    assert!(
        details
            .nodes
            .iter()
            .all(|n| tail.nodes.iter().all(|m| n.id != m.id))
    );
    let root = manager
        .operator_graph_node(OperatorGraphNodeQuery {
            id: prepared.graph_id.clone(),
            node_id: "root".into(),
        })
        .await
        .unwrap();
    assert_eq!(root.status, "dispatched");
    assert_eq!(root.delivery_state, "pending_binding");
    assert!(!root.runtime_delivery_authorized);
    let exposed = serde_json::to_value(&root).unwrap();
    for private in ["message_id", "messageId", "viewToken", "schema", "document"] {
        assert!(exposed.get(private).is_none());
    }
    let stale = manager
        .prepare_operator_graph_cancel(OperatorGraphCancel {
            id: prepared.graph_id.clone(),
            expected_revision: 1,
        })
        .await
        .unwrap();
    let result_json = json!({"score":0.75,"large":9007199254740993u64,"opaque":{"__proto__":"retained"},"report":"x".repeat(20000)});
    let update:OperatorGraphUpdate=serde_json::from_value(json!({"id":prepared.graph_id,"expectedRevision":1,"patch":{"nodeId":"root","status":"complete","resultJson":serde_json::to_string(&result_json).unwrap()}})).unwrap();
    let update = manager.prepare_operator_graph_update(update).await.unwrap();
    let review = manager
        .review_saved_operator_graph(update.command_id.clone())
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&review.change).unwrap()["patch"]["result"],
        result_json
    );
    let applied = manager
        .apply_saved_operator_graph(update.command_id.clone())
        .await
        .unwrap();
    assert_eq!(applied.revision, 2);
    assert_eq!(applied.queued_messages, 18);
    let resumed = manager
        .retry_saved_command(update.command_id.clone())
        .await
        .unwrap();
    assert_eq!(resumed["operation"], "graph_update_node");
    assert_eq!(resumed["data"], serde_json::to_value(&applied).unwrap());
    assert_eq!(resumed.as_object().unwrap().len(), 2);
    assert_eq!(counts(&f), (1, 19, 19, 0));
    let node = manager
        .operator_graph_node(OperatorGraphNodeQuery {
            id: prepared.graph_id.clone(),
            node_id: "root".into(),
        })
        .await
        .unwrap();
    assert_eq!(node.result, result_json);
    assert_eq!(
        serde_json::from_str::<Value>(&node.result_json).unwrap(),
        result_json
    );
    assert_eq!(
        serde_json::from_str::<Value>(&review.change_json).unwrap()["patch"]["result"],
        result_json
    );
    let error = manager
        .apply_saved_operator_graph(stale.command_id.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, "management_revision_conflict");
    assert_eq!(
        manager
            .review_saved_operator_graph(stale.command_id.clone())
            .await
            .unwrap()
            .error
            .as_deref(),
        Some("management_revision_conflict")
    );
    manager.acknowledge_command(stale.command_id).await.unwrap();
    let cancelled = manager
        .prepare_operator_graph_cancel(OperatorGraphCancel {
            id: prepared.graph_id.clone(),
            expected_revision: 2,
        })
        .await
        .unwrap();
    let cancelled = manager
        .apply_saved_operator_graph(cancelled.command_id)
        .await
        .unwrap();
    assert_eq!(cancelled.revision, 3);
    assert_eq!(cancelled.suppressed_messages, 19);
    assert_eq!(cancelled.delivery_state, "suppressed");
    assert_eq!(counts(&f), (1, 19, 0, 0));
    // Historical replay returns the original effect receipt after later edits.
    assert_eq!(
        manager
            .apply_saved_operator_graph(prepared.command_id.clone())
            .await
            .unwrap(),
        first
    );
    let mut readonly_package = package(4);
    readonly_package.permissions = vec![
        Permission::ReadOperatorGraphs,
        Permission::ReadIntents,
        Permission::AcknowledgeIntents,
    ];
    let readonly = f.owner.management(readonly_package).await.unwrap();
    assert_eq!(
        readonly
            .operator_graph_page(Default::default())
            .await
            .unwrap()
            .graphs[0]
            .status,
        "cancelled"
    );
    assert!(
        readonly
            .review_saved_operator_graph(prepared.command_id.clone())
            .await
            .is_err()
    );
    assert!(
        readonly
            .apply_saved_operator_graph(prepared.command_id.clone())
            .await
            .is_err()
    );
    assert!(
        readonly
            .retry_saved_command(prepared.command_id.clone())
            .await
            .is_err()
    );
    manager
        .acknowledge_command(prepared.command_id.clone())
        .await
        .unwrap();
    assert!(
        manager
            .saved_commands()
            .await
            .unwrap()
            .iter()
            .all(|c| c.command_id != prepared.command_id)
    );
    f.live.store(false, Ordering::Release);
    assert!(
        manager
            .operator_graph_page(Default::default())
            .await
            .is_err()
    );
    assert!(
        manager
            .prepare_operator_graph_create(create_input())
            .await
            .is_err()
    );
    assert!(
        manager
            .review_saved_operator_graph(update.command_id)
            .await
            .is_err()
    );
    assert_eq!(counts(&f), (1, 19, 0, 0));
    drop(readonly);
    drop(manager);
    f.stop().await;
    registry.shutdown().await.unwrap();
}

#[test]
fn native_graph_json_drafts_preserve_null_omission_and_fractional_data() {
    let base = json!({"id":"graph","expectedRevision":1,"patch":{"nodeId":"a","status":"active"}});
    let absent: OperatorGraphUpdate = serde_json::from_value(base.clone()).unwrap();
    let (_, _, change) = absent.change().unwrap();
    assert!(
        serde_json::to_value(change).unwrap()["patch"]
            .get("result")
            .is_none()
    );
    let mut explicit = base;
    explicit["patch"]["resultJson"] = json!("null");
    let (_, _, change) = serde_json::from_value::<OperatorGraphUpdate>(explicit)
        .unwrap()
        .change()
        .unwrap();
    assert_eq!(
        serde_json::to_value(change).unwrap()["patch"].get("result"),
        Some(&Value::Null)
    );
    let value = json!({"fraction":0.25,"large":9007199254740993u64,"__proto__":{"preserved":true}});
    let input:OperatorGraphUpdate=serde_json::from_value(json!({"id":"graph","expectedRevision":1,"patch":{"nodeId":"a","resultJson":serde_json::to_string(&value).unwrap()}})).unwrap();
    let (_, _, change) = input.change().unwrap();
    assert_eq!(
        serde_json::to_value(change).unwrap()["patch"]["result"],
        value
    );
}
#[test]
fn native_graph_invalid_drafts_are_refused_before_custody() {
    for patch in [
        json!({"nodeId":"a","resultJson":"{broken"}),
        json!({"nodeId":"a","resultJson":"{\"a\":1,\"a\":2}"}),
        json!({"nodeId":"a","resultJson":"null","result":null}),
        json!({"nodeId":"a","resultJson":serde_json::to_string(&"x".repeat(65535)).unwrap()}),
        json!({"nodeId":"a","error":"x".repeat(4001)}),
        json!({"nodeId":"a","status":"running"}),
        json!({"nodeId":"a"}),
    ] {
        let input: OperatorGraphUpdate =
            serde_json::from_value(json!({"id":"graph","expectedRevision":1,"patch":patch}))
                .unwrap();
        assert!(input.validate().is_err());
    }
    for nodes in [
        json!({"a":{"assignee":"one","description":"work","dependsOn":["a"]}}),
        json!({"a":{"assignee":"one","description":"work","condition":null,"conditionJson":"null"}}),
        json!({"a":{"assignee":"one","description":"work","conditionJson":"[]"}}),
        json!({"a\nb":{"assignee":"one","description":"work"}}),
    ] {
        let input: OperatorGraphCreate =
            serde_json::from_value(json!({"label":"Bad","nodes":nodes})).unwrap();
        assert!(input.validate().is_err());
    }
    assert!(
        serde_json::from_value::<OperatorGraphCreate>(
            json!({"label":"Bad","owner":"@other:test","nodes":{}})
        )
        .is_err()
    );
}
#[test]
fn native_graph_failure_classification_never_finalizes_authentication_or_unknown_errors() {
    for (status, code) in [
        (401, "management_access_required"),
        (409, "management_outcome_unknown"),
        (503, "management_unavailable"),
        (400, "unrelated_error"),
    ] {
        assert!(!known_failure(&failure(status, code)));
    }
    assert!(known_failure(&failure(409, "management_revision_conflict")));
}
