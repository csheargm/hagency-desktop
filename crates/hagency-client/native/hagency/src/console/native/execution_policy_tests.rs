//! Native policy persistence through real loopback identity and owner restart.
use super::*;
use hagency_store::management::{
    Intent, IntentOperation, EditExecutionPolicy, ExecutionPolicyQuery, ExecutionPolicyValue,
};

fn package() -> ManagementPackage {
    ManagementPackage {
        digest: "e".repeat(64),
        instance: 1048,
        account_epoch: 1,
        permissions: vec![
            Permission::ReadExecutionPolicy,
            Permission::EditExecutionPolicy,
            Permission::ReadIntents,
            Permission::AcknowledgeIntents,
        ],
    }
}
async fn fixture() -> (Fixture, ExecutionPolicyQuery) {
    let (f, resource, room) = approval_bindings::fixture().await;
    (
        approval_bindings::reopen(f).await,
        ExecutionPolicyQuery {
            resource_id: resource,
            engagement_id: room.engagement_id,
        },
    )
}
async fn selected(
    m: &NativeManagement,
    query: ExecutionPolicyQuery,
    id: &str,
) -> EditExecutionPolicy {
    let row = m.agent_execution_policy(query).await.unwrap();
    EditExecutionPolicy {
        web_search: None,
        command_id: id.into(),
        resource_id: row.resource_id,
        engagement_id: row.engagement_id,
        expected_revision: row.revision,
        execution_policy: ExecutionPolicyValue { yolo: true },
    }
}
#[tokio::test]
async fn native_execution_policy_lost_reply_recovers_after_restart_and_rechecks_authority() {
    let (mut f, query) = fixture().await;
    let m = f.owner.management(package()).await.unwrap();
    let input = selected(&m, query.clone(), "original_policy").await;
    let (ctx, session) = m.authorized().await.unwrap();
    session
        .prepare_intent(
            &ctx,
            Intent::new(
                IntentOperation::EditExecutionPolicy,
                input.command_id.clone(),
                &input,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let original = session
        .edit_execution_policy(&ctx, input.clone())
        .await
        .unwrap();
    assert!(
        m.saved_commands()
            .await
            .unwrap()
            .iter()
            .find(|r| r.command_id == input.command_id)
            .unwrap()
            .reply
            .is_none()
    );
    let mut later = selected(&m, query.clone(), "later_policy").await;
    later.execution_policy.yolo = false;
    m.edit_execution_policy(later).await.unwrap();
    drop(session);
    drop(m);
    f = approval_bindings::reopen(f).await;
    let m = f.owner.management(package()).await.unwrap();
    let reply = m
        .retry_saved_command(input.command_id.clone())
        .await
        .unwrap();
    assert_eq!(reply["operation"], "edit_execution_policy");
    assert_eq!(reply["data"], serde_json::to_value(&original).unwrap());
    assert!(
        !m.agent_execution_policy(query.clone())
            .await
            .unwrap()
            .execution_policy
            .yolo
    );
    assert_eq!(
        m.retry_saved_command(input.command_id.clone())
            .await
            .unwrap()["data"],
        serde_json::to_value(&original).unwrap()
    );
    let mut reader = package();
    reader.instance += 1;
    reader
        .permissions
        .retain(|p| *p != Permission::EditExecutionPolicy);
    assert!(
        f.owner
            .management(reader)
            .await
            .unwrap()
            .retry_saved_command(input.command_id.clone())
            .await
            .is_err()
    );
    f.current.store(false, Ordering::Release);
    assert!(
        m.retry_saved_command(input.command_id.clone())
            .await
            .is_err()
    );
    f.current.store(true, Ordering::Release);
    assert_eq!(m.edit_execution_policy(input).await.unwrap(), original);
    assert!(!m.agent_execution_policy(query).await.unwrap().execution_policy.yolo);
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
#[tokio::test]
async fn native_execution_policy_conflict_does_not_settle_an_unknown_attempt() {
    let (f, query) = fixture().await;
    let m = f.owner.management(package()).await.unwrap();
    let mut input = selected(&m, query.clone(), "stale_policy").await;
    input.expected_revision = "b".repeat(64);
    assert_eq!(
        m.edit_execution_policy(input.clone())
            .await
            .unwrap_err()
            .code,
        "management_revision_conflict"
    );
    assert!(
        m.saved_commands()
            .await
            .unwrap()
            .iter()
            .any(|r| r.command_id == input.command_id && r.reply.is_some())
    );
    input.command_id = "unknown_policy".into();
    let (ctx, session) = m.authorized().await.unwrap();
    session
        .prepare_intent(
            &ctx,
            Intent::new(
                IntentOperation::EditExecutionPolicy,
                input.command_id.clone(),
                &input,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        m.edit_execution_policy(input.clone())
            .await
            .unwrap_err()
            .code,
        "management_outcome_unknown"
    );
    assert!(
        m.saved_commands()
            .await
            .unwrap()
            .iter()
            .any(|r| r.command_id == input.command_id && r.reply.is_none())
    );
    assert!(
        !m.agent_execution_policy(query)
            .await
            .unwrap()
            .execution_policy
            .yolo
    );
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
