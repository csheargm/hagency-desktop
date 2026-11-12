use super::*;
use hagency_store::management::TaskField;
use std::time::{Duration, Instant};


fn profile(ch: char, mxid: &str) -> ProfileAuthority {
    ProfileAuthority::new(
        ch.to_string().repeat(64),
        mxid.into(),
        Instant::now() + Duration::from_secs(120),
    )
    .unwrap()
}
fn create() -> OperatorGraphChange {
    serde_json::from_value(json!({"operation":"create","label":"Review then implement","nodes":{
        "inspect":{"assignee":"Recorded reviewer","description":"Inspect changes"},
        "apply":{"assignee":"Recorded builder","description":"Implement approved change","dependsOn":["inspect"],"condition":{"dep":"inspect","path":"approved","eq":true}},
        "alternative":{"assignee":"Recorded writer","description":"Explain refusal","dependsOn":["inspect"],"condition":{"dep":"inspect","path":"approved","eq":false}}
    }})).unwrap()
}
fn patch(id: &str, status: &str, result: Value) -> OperatorGraphChange {
    OperatorGraphChange::UpdateNode {
        patch: OperatorGraphNodePatch {
            node_id: id.into(),
            status: Some(status.into()),
            result: TaskField::Set(Some(result)),
            error: TaskField::Unchanged,
        },
    }
}

#[tokio::test]
async fn canonical_graph_commands_commit_branches_and_recover_original_receipts_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("native");
    let registry = OwnerResourceRegistry::isolated_for_tests(&root).unwrap();
    let p = profile('a', "@manager:example.test");
    let journal = OperatorGraphJournal::new(registry.clone(), p, "native_scope".into()).unwrap();
    let first = journal
        .apply("create_graph".into(), "graph_原件".into(), 0, create())
        .await
        .unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(first.queued_messages, 1);
    let initial = journal.read("graph_原件".into()).await.unwrap().unwrap();
    assert!(!initial.runtime_delivery_authorized);
    assert_eq!(initial.graph["owner"], "@manager:example.test");
    assert_eq!(initial.graph["nodes"]["inspect"]["status"], "dispatched");
    assert_eq!(initial.graph["nodes"]["apply"]["status"], "pending");
    let original_message = initial.graph["nodes"]["inspect"]["message_id"].clone();
    let completed = journal
        .apply(
            "finish_inspection".into(),
            "graph_原件".into(),
            1,
            patch(
                "inspect",
                "complete",
                json!({"approved":true,"report":"preserve exact result"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(completed.revision, 2);
    assert_eq!(completed.queued_messages, 1);
    let selected = journal.read("graph_原件".into()).await.unwrap().unwrap();
    assert_eq!(
        selected.graph["nodes"]["inspect"]["message_id"],
        original_message
    );
    assert_eq!(selected.graph["nodes"]["apply"]["status"], "dispatched");
    assert_eq!(selected.graph["nodes"]["alternative"]["status"], "skipped");
    let cancel = journal
        .apply(
            "cancel_graph".into(),
            "graph_原件".into(),
            2,
            OperatorGraphChange::Cancel,
        )
        .await
        .unwrap();
    assert_eq!(cancel.revision, 3);
    assert_eq!(cancel.queued_messages, 0);
    assert_eq!(cancel.suppressed_messages, 2);
    let end = journal.read("graph_原件".into()).await.unwrap().unwrap();
    assert_eq!(end.graph["status"], "cancelled");
    assert_eq!(end.graph["nodes"]["inspect"]["status"], "complete");
    assert_eq!(
        end.graph["nodes"]["inspect"]["result"]["report"],
        "preserve exact result"
    );
    assert_eq!(end.graph["nodes"]["apply"]["status"], "cancelled");
    assert_eq!(
        journal
            .apply("create_graph".into(), "graph_原件".into(), 0, create())
            .await
            .unwrap(),
        first
    );
    drop(journal);
    registry.shutdown().await.unwrap();
    drop(registry);
    let registry = OwnerResourceRegistry::isolated_for_tests(&root).unwrap();
    let journal = OperatorGraphJournal::new(
        registry.clone(),
        profile('a', "@manager:example.test"),
        "native_scope".into(),
    )
    .unwrap();
    assert_eq!(
        journal
            .apply("create_graph".into(), "graph_原件".into(), 0, create())
            .await
            .unwrap(),
        first
    );
    let restored = journal.read("graph_原件".into()).await.unwrap().unwrap();
    assert_eq!(restored.revision, 3);
    assert_eq!(restored.graph, end.graph);
    drop(journal);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_graph_and_oversized_patch_leave_no_partial_state_or_new_revision() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::isolated_for_tests(&temp.path().join("native")).unwrap();
    let journal = OperatorGraphJournal::new(
        registry.clone(),
        profile('a', "@manager:example.test"),
        "native_scope".into(),
    )
    .unwrap();
    let mut invalid = create();
    if let OperatorGraphChange::Create { nodes, .. } = &mut invalid {
        nodes
            .get_mut("inspect")
            .unwrap()
            .depends_on
            .push("apply".into());
    }
    assert!(
        journal
            .apply("bad".into(), "bad_graph".into(), 0, invalid)
            .await
            .is_err()
    );
    assert!(journal.read("bad_graph".into()).await.unwrap().is_none());
    let first = journal
        .apply("create".into(), "graph".into(), 0, create())
        .await
        .unwrap();
    let before = journal.read("graph".into()).await.unwrap().unwrap();
    assert!(
        journal
            .apply(
                "oversized".into(),
                "graph".into(),
                1,
                patch("inspect", "complete", json!("x".repeat(65_536)))
            )
            .await
            .is_err()
    );
    let after = journal.read("graph".into()).await.unwrap().unwrap();
    assert_eq!(after.revision, 1);
    assert_eq!(after.graph, before.graph);
    assert!(
        journal
            .apply(
                "create".into(),
                "graph".into(),
                0,
                OperatorGraphChange::Create {
                    label: "Changed command".into(),
                    nodes: match create() {
                        OperatorGraphChange::Create { nodes, .. } => nodes,
                        _ => unreachable!(),
                    }
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        journal
            .apply("create".into(), "graph".into(), 0, create())
            .await
            .unwrap(),
        first
    );
    drop(journal);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn graph_scopes_and_accounts_do_not_reinterpret_recorded_assignees_as_authority() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::isolated_for_tests(&temp.path().join("native")).unwrap();
    let first = OperatorGraphJournal::new(
        registry.clone(),
        profile('a', "@manager:example.test"),
        "native_scope".into(),
    )
    .unwrap();
    first
        .apply("create".into(), "graph".into(), 0, create())
        .await
        .unwrap();
    let other = OperatorGraphJournal::new(
        registry.clone(),
        profile('b', "@other:example.test"),
        "native_scope".into(),
    )
    .unwrap();
    assert!(other.read("graph".into()).await.unwrap().is_none());
    assert!(
        other
            .apply(
                "cancel".into(),
                "graph".into(),
                1,
                OperatorGraphChange::Cancel
            )
            .await
            .is_err()
    );
    let same_digest_wrong_owner = OperatorGraphJournal::new(
        registry.clone(),
        profile('a', "@other:example.test"),
        "native_scope".into(),
    )
    .unwrap();
    assert!(
        same_digest_wrong_owner
            .read("graph".into())
            .await
            .unwrap()
            .is_none()
    );
    let scope = OperatorGraphJournal::new(
        registry.clone(),
        profile('a', "@manager:example.test"),
        "different_scope".into(),
    )
    .unwrap();
    assert!(scope.read("graph".into()).await.unwrap().is_none());
    let record = first.read("graph".into()).await.unwrap().unwrap();
    assert_eq!(record.revision, 1);
    assert!(!record.runtime_delivery_authorized);
    drop((first, other, same_digest_wrong_owner, scope));
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn exact_unicode_node_ids_and_nullable_patch_members_survive() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::isolated_for_tests(&temp.path().join("native")).unwrap();
    let journal = OperatorGraphJournal::new(
        registry.clone(),
        profile('a', "@manager:example.test"),
        "scope".into(),
    )
    .unwrap();
    let id = "雪".repeat(255);
    let change = OperatorGraphChange::Create {
        label: "Unicode".into(),
        nodes: BTreeMap::from([(
            id.clone(),
            OperatorGraphNodeInput {
                assignee: "Recorded label".into(),
                description: "Work".into(),
                depends_on: vec![],
                condition: None,
            },
        )]),
    };
    journal
        .apply("create".into(), "graph".into(), 0, change)
        .await
        .unwrap();
    journal
        .apply(
            "result".into(),
            "graph".into(),
            1,
            patch(&id, "active", json!({"first":true})),
        )
        .await
        .unwrap();
    let clear: OperatorGraphChange = serde_json::from_value(
        json!({"operation":"update_node","patch":{"nodeId":id,"result":null}}),
    )
    .unwrap();
    journal
        .apply("clear".into(), "graph".into(), 2, clear)
        .await
        .unwrap();
    let graph = journal.read("graph".into()).await.unwrap().unwrap().graph;
    assert!(graph["nodes"][&id]["result"].is_null());
    assert_eq!(graph["nodes"][&id]["status"], "active");
    assert_eq!(graph["nodes"].as_object().unwrap().len(), 1);
    drop(journal);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn fractional_and_opaque_json_results_survive_atomic_updates_and_exact_replay() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::isolated_for_tests(&temp.path().join("native")).unwrap();
    let journal = OperatorGraphJournal::new(
        registry.clone(),
        profile('a', "@manager:example.test"),
        "scope".into(),
    )
    .unwrap();
    let mut graph = create();
    if let OperatorGraphChange::Create { nodes, .. } = &mut graph {
        nodes.get_mut("apply").unwrap().condition =
            Some(json!({"dep":"inspect","path":"score","eq":0.75}));
    }
    journal
        .apply("create".into(), "graph".into(), 0, graph)
        .await
        .unwrap();
    let result = json!({"score":0.75,"large":u64::MAX,"__proto__":{"retain":"opaque data"}});
    let change = patch("inspect", "complete", result.clone());
    let receipt = journal
        .apply("finish".into(), "graph".into(), 1, change.clone())
        .await
        .unwrap();
    assert_eq!(receipt.queued_messages, 1);
    let current = journal.read("graph".into()).await.unwrap().unwrap();
    assert_eq!(current.graph["nodes"]["inspect"]["result"], result);
    assert_eq!(current.graph["nodes"]["apply"]["status"], "dispatched");
    assert_eq!(
        journal
            .apply("finish".into(), "graph".into(), 1, change)
            .await
            .unwrap(),
        receipt
    );
    assert!(
        journal
            .apply(
                "finish".into(),
                "graph".into(),
                1,
                patch("inspect", "complete", json!({"score":0.5}))
            )
            .await
            .is_err()
    );
    drop(journal);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn undeliverable_control_character_node_ids_are_rejected_before_root_queueing() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::isolated_for_tests(&temp.path().join("native")).unwrap();
    let journal = OperatorGraphJournal::new(
        registry.clone(),
        profile('a', "@manager:example.test"),
        "scope".into(),
    )
    .unwrap();
    for (i, control) in ["later\nnode", "later\tnode", "later\u{7f}node"]
        .iter()
        .enumerate()
    {
        let mut graph = create();
        if let OperatorGraphChange::Create { nodes, .. } = &mut graph {
            nodes.insert(
                (*control).into(),
                OperatorGraphNodeInput {
                    assignee: "Recorded builder".into(),
                    description: "Later work".into(),
                    depends_on: vec!["inspect".into()],
                    condition: None,
                },
            );
        }
        assert!(
            journal
                .apply(format!("create{i}"), format!("graph{i}"), 0, graph)
                .await
                .is_err()
        );
        assert!(journal.read(format!("graph{i}")).await.unwrap().is_none());
    }
    drop(journal);
    registry.shutdown().await.unwrap();
}
