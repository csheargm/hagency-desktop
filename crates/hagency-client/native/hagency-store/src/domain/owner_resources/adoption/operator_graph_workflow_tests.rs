use super::*;
use crate::management::{
    HostContext, Intent, IntentOperation, IntentReply, Permission,
    Session, OPERATOR_GRAPH_SCOPE,
};
use crate::owner_resources::graph_journal::GraphJournalCommand;

fn context(f: &Fixture, epoch: u64) -> HostContext {
    HostContext::new(
        f.profile.account.clone(),
        epoch,
        "a".repeat(64),
        epoch,
        "graph_device".into(),
        1,
    )
    .unwrap()
}
fn session(
    backing: &CurrentManagementStore,
    context: &HostContext,
    permissions: &[Permission],
) -> Session {
    Session::new_current(
        backing.clone(),
        context.clone(),
        permissions.iter().copied(),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
const GRANTS: &[Permission] = &[
    Permission::ReadOperatorGraphs,
    Permission::CreateOperatorGraphs,
    Permission::UpdateOperatorGraphNodes,
    Permission::CancelOperatorGraphs,
    Permission::ReadOperatorGraphRecipients,
    Permission::BindOperatorGraphRecipients,
    Permission::ReadIntents,
    Permission::AcknowledgeIntents,
];

#[tokio::test]
async fn graph_access_retains_exact_grant_account_epoch_and_backing_gate() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f, 1);
    for rights in [
        vec![],
        vec![
            Permission::ReadTasks,
            Permission::CreateTasks,
        ],
    ] {
        let denied = session(&backing, &ctx, &rights);
        for permission in &GRANTS[..6] {
            assert!(matches!(
                denied.operator_graph_access(&ctx, *permission),
                Err(Error::LocalAuthority)
            ));
        }
    }
    for permission in &GRANTS[..6] {
        let manager = session(&backing, &ctx, &[*permission]);
        let (registry, proof) = manager.operator_graph_access(&ctx, *permission).unwrap();
        assert_eq!(registry.id(), f.registry.id());
        for other in GRANTS[..6].iter().filter(|other| *other != permission) {
            assert!(manager.operator_graph_access(&ctx, *other).is_err());
        }
        assert!(
            manager
                .operator_graph_access(&context(&f, 2), *permission)
                .is_err()
        );
        assert!(
            manager
                .operator_graph_access(&ctx, Permission::ReadTasks)
                .is_err()
        );
        let other = HostContext::new(
            "@other:test".into(),
            1,
            "a".repeat(64),
            1,
            "graph_device".into(),
            1,
        )
        .unwrap();
        assert!(manager.operator_graph_access(&other, *permission).is_err());
        manager.retire();
        assert!(matches!(proof.check_current(), Err(Error::LocalAuthority)));
        assert!(
            registry
                .operator_graph_record_page(&proof, OPERATOR_GRAPH_SCOPE, None)
                .await
                .is_err()
        );
    }
    let manager = session(&backing, &ctx, GRANTS);
    let (_, proof) = manager
        .operator_graph_access(&ctx, Permission::CreateOperatorGraphs)
        .unwrap();
    backing.shutdown().await.unwrap();
    assert!(proof.check_current().is_err());
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn graph_record_index_is_bounded_scoped_and_cursor_exact() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f, 1);
    let manager = session(&backing, &ctx, GRANTS);
    let (registry, proof) = manager
        .operator_graph_access(&ctx, Permission::CreateOperatorGraphs)
        .unwrap();
    for n in 0..18 {
        registry
            .transact_operator_graph(
                &proof,
                GraphJournalCommand {
                    command_id: format!("create_{n}"),
                    scope_id: OPERATOR_GRAPH_SCOPE.into(),
                    graph_id: format!("graph_{n:02}"),
                    expected_revision: 0,
                    input_digest: "a".repeat(64),
                },
                move |_, _| Ok(vec![n; 4 * 1024]),
            )
            .await
            .unwrap();
    }
    let (_, read) = manager
        .operator_graph_access(&ctx, Permission::ReadOperatorGraphs)
        .unwrap();
    let first = registry
        .operator_graph_record_page(&read, OPERATOR_GRAPH_SCOPE, None)
        .await
        .unwrap();
    assert_eq!(first.items.len(), 16);
    assert_eq!(first.next.as_deref(), Some("graph_15"));
    assert!(
        first
            .items
            .iter()
            .all(|v| v.revision == 1 && v.document_bytes == 4096)
    );
    let last = registry
        .operator_graph_record_page(&read, OPERATOR_GRAPH_SCOPE, first.next)
        .await
        .unwrap();
    assert_eq!(
        last.items
            .iter()
            .map(|v| v.graph_id.as_str())
            .collect::<Vec<_>>(),
        vec!["graph_16", "graph_17"]
    );
    assert!(last.next.is_none());
    assert!(
        registry
            .operator_graph_record_page(&read, "different_scope", None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let other = ProfileAuthority::new(
        "b".repeat(64),
        "@other:test".into(),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        registry
            .operator_graph_record_page(&other, OPERATOR_GRAPH_SCOPE, None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    manager.retire();
    assert!(
        registry
            .operator_graph_record_page(&read, OPERATOR_GRAPH_SCOPE, None)
            .await
            .is_err()
    );
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

fn graph_intent(id: &str, operation: IntentOperation) -> Intent {
    let padding = match operation {
        IntentOperation::GraphCreate => 64 * 1024,
        IntentOperation::GraphUpdateNode => 128 * 1024,
        IntentOperation::GraphBindRecipient => 32 * 1024,
        _ => 0,
    };
    Intent::new(operation,id.into(),&json!({"commandId":id,"graphId":"graph","opaque":"x".repeat(padding),"result":{"fraction":0.125,"array":[null,1.75]},"expectedRevision":1})).unwrap()
}

#[tokio::test]
async fn graph_intent_large_fractional_custody_restarts_with_eight_records_and_exact_permissions() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f, 1);
    let manager = session(&backing, &ctx, GRANTS);
    let operations = [
        IntentOperation::GraphCreate,
        IntentOperation::GraphUpdateNode,
        IntentOperation::GraphCancel,
        IntentOperation::GraphBindRecipient,
        IntentOperation::GraphUpdateNode,
        IntentOperation::GraphUpdateNode,
        IntentOperation::GraphUpdateNode,
        IntentOperation::GraphUpdateNode,
    ];
    for (index, operation) in operations.into_iter().enumerate() {
        let id = format!("original_{index}");
        let row = manager
            .prepare_intent(&ctx, graph_intent(&id, operation))
            .await
            .unwrap();
        assert!(row.is_first_attempt());
        if index != 1 {
            manager
                .record_intent_reply(
                    &ctx,
                    graph_intent(&id, operation),
                    IntentReply::Success(
                        json!({"revision":1,"fraction":0.125,"opaque":"r".repeat(60*1024)}),
                    ),
                )
                .await
                .unwrap();
        }
    }
    assert!(matches!(
        manager
            .prepare_intent(&ctx, graph_intent("ninth", IntentOperation::GraphCancel))
            .await,
        Err(Error::Capacity)
    ));
    let pending = manager.pending_intents(&ctx).await.unwrap();
    assert_eq!(pending.len(), 8);
    assert!(serde_json::to_vec(&pending).unwrap().len() > 1024 * 1024);
    assert!(matches!(
        manager.acknowledge_intent(&ctx, "original_1".into()).await,
        Err(Error::Conflict)
    ));
    manager.retire();
    backing.shutdown().await.unwrap();
    drop(manager);
    drop(backing);
    let reopened = current_backing(&f).await;
    let renewed = context(&f, 2);
    let manager = session(&reopened, &renewed, GRANTS);
    let recovered = manager.pending_intents(&renewed).await.unwrap();
    assert_eq!(
        serde_json::to_value(&pending).unwrap(),
        serde_json::to_value(&recovered).unwrap()
    );
    for (index, operation) in operations.into_iter().enumerate() {
        let id = format!("original_{index}");
        let recovered = manager
            .prepare_intent(&renewed, graph_intent(&id, operation))
            .await
            .unwrap();
        assert!(!recovered.is_first_attempt());
        assert_eq!(recovered.operation, operation);
        assert_eq!(recovered.input["result"]["fraction"], json!(0.125));
    }
    let denied = session(
        &reopened,
        &renewed,
        &[
            Permission::ReadIntents,
            Permission::CreateTasks,
            Permission::AcknowledgeIntents,
        ],
    );
    assert!(denied.pending_intents(&renewed).await.unwrap().is_empty());
    assert!(
        denied
            .prepare_intent(
                &renewed,
                graph_intent("original_0", IntentOperation::GraphCreate)
            )
            .await
            .is_err()
    );
    assert!(
        denied
            .acknowledge_intent(&renewed, "original_0".into())
            .await
            .is_err()
    );
    let creator = session(
        &reopened,
        &renewed,
        &[Permission::ReadIntents, Permission::CreateOperatorGraphs],
    );
    assert_eq!(creator.pending_intents(&renewed).await.unwrap().len(), 1);
    manager
        .acknowledge_intent(&renewed, "original_0".into())
        .await
        .unwrap();
    assert_eq!(manager.pending_intents(&renewed).await.unwrap().len(), 7);
    reopened.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
