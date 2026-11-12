//! Actual NativeOwner restart and loopback identity; synthetic stored rule.
use super::*;
use hagency_store::management::{Intent, IntentOperation, RevokeToolGrant, ToolGrantQuery};

fn package() -> ManagementPackage {
    ManagementPackage {
        digest: "e".repeat(64),
        instance: 1048,
        account_epoch: 1,
        permissions: vec![
            Permission::ReadToolApprovals,
            Permission::InspectToolGrants,
            Permission::RevokeToolGrants,
            Permission::ReadIntents,
            Permission::AcknowledgeIntents,
        ],
    }
}
async fn fixture() -> (Fixture, ToolGrantQuery) {
    let (f, resource, room) = approval_bindings::fixture().await;
    let path = f._root.path().join("registry/domain.sqlite3");
    let sql = rusqlite::Connection::open(path).unwrap();
    // Transport/recovery fixture only. Real request/verdict-to-rule creation is
    // covered by the store approvals integration suite.
    sql.execute("INSERT INTO approval_grants(id,engagement_id,binding_generation,scope_key,scope_kind,mode,context_key) VALUES('grant_fixture',?1,1,'scope_fixture','\"exact_command\"','always','context_fixture')",[&room.engagement_id]).unwrap();
    drop(sql);
    (
        approval_bindings::reopen(f).await,
        ToolGrantQuery {
            resource_id: resource,
            engagement_id: room.engagement_id,
            after: String::new(),
        },
    )
}
async fn selected(m: &NativeManagement, query: ToolGrantQuery, id: &str) -> RevokeToolGrant {
    let row = m.tool_grants(query).await.unwrap().grants.remove(0);
    RevokeToolGrant {
        command_id: id.into(),
        resource_id: row.resource_id,
        engagement_id: row.engagement_id,
        grant_id: row.id,
        expected_revision: row.revision,
    }
}
#[tokio::test]
async fn native_tool_grant_lost_reply_recovers_after_restart_and_rechecks_authority() {
    let (mut f, query) = fixture().await;
    let m = f.owner.management(package()).await.unwrap();
    let input = selected(&m, query.clone(), "original_revoke").await;
    let (ctx, session) = m.authorized().await.unwrap();
    session
        .prepare_intent(
            &ctx,
            Intent::new(
                IntentOperation::RevokeToolGrant,
                input.command_id.clone(),
                &input,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let original = session
        .revoke_tool_grant(&ctx, input.clone())
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
    drop(session);
    drop(m);
    f = approval_bindings::reopen(f).await;
    let m = f.owner.management(package()).await.unwrap();
    let reply = m
        .retry_saved_command(input.command_id.clone())
        .await
        .unwrap();
    assert_eq!(reply["operation"], "revoke_tool_grant");
    assert_eq!(reply["data"], serde_json::to_value(&original).unwrap());
    assert!(m.tool_grants(query.clone()).await.unwrap().grants[0].revoked);
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
        .retain(|p| *p != Permission::RevokeToolGrants);
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
    assert_eq!(m.revoke_tool_grant(input).await.unwrap(), original);
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
#[tokio::test]
async fn native_tool_grant_conflict_does_not_settle_an_unknown_attempt() {
    let (f, query) = fixture().await;
    let m = f.owner.management(package()).await.unwrap();
    let mut input = selected(&m, query.clone(), "stale_revoke").await;
    input.expected_revision = "b".repeat(64);
    assert_eq!(
        m.revoke_tool_grant(input.clone()).await.unwrap_err().code,
        "management_revision_conflict"
    );
    assert!(
        m.saved_commands()
            .await
            .unwrap()
            .iter()
            .any(|r| r.command_id == input.command_id && r.reply.is_some())
    );
    input.command_id = "unknown_revoke".into();
    let (ctx, session) = m.authorized().await.unwrap();
    session
        .prepare_intent(
            &ctx,
            Intent::new(
                IntentOperation::RevokeToolGrant,
                input.command_id.clone(),
                &input,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        m.revoke_tool_grant(input.clone()).await.unwrap_err().code,
        "management_outcome_unknown"
    );
    assert!(
        m.saved_commands()
            .await
            .unwrap()
            .iter()
            .any(|r| r.command_id == input.command_id && r.reply.is_none())
    );
    assert!(!m.tool_grants(query).await.unwrap().grants[0].revoked);
    f.owner.suspend().await.unwrap();
    f.stop().await;
}

#[tokio::test]
async fn native_tool_grant_receipt_failure_rolls_back_the_current_store() {
    let (f, query) = fixture().await;
    let m = f.owner.management(package()).await.unwrap();
    let before = m.tool_grants(query.clone()).await.unwrap().grants.remove(0);
    let input = selected(&m, query.clone(), "receipt_failure").await;
    let (ctx, session) = m.authorized().await.unwrap();
    let sql = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_tool_receipt BEFORE INSERT ON management_commands WHEN NEW.operation='tool_grant.revoke' BEGIN SELECT RAISE(ABORT,'fixture receipt failure'); END;").unwrap();
    assert!(session.revoke_tool_grant(&ctx, input.clone()).await.is_err());
    assert_eq!(m.tool_grants(query).await.unwrap().grants[0], before);
    assert_eq!(sql.query_row("SELECT count(*) FROM management_commands WHERE operation='tool_grant.revoke'", [], |r| r.get::<_, u64>(0)).unwrap(), 0);
    sql.execute_batch("DROP TRIGGER fail_tool_receipt").unwrap();
    assert!(session.revoke_tool_grant(&ctx, input).await.unwrap().revoked);
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
