//! Real loopback identity and SQLite, synthetic rooms, no provider inference.
use super::*;
use hagency_store::management::{
    Intent, IntentOperation, ResourceApprovalBindingQuery, RetireResourceApprovalBinding,
};
use crate::file_service::test_common::domain as legacy;

fn package() -> ManagementPackage {
    ManagementPackage {
        digest: "e".repeat(64),
        instance: 1047,
        account_epoch: 1,
        permissions: vec![
            Permission::ReadApprovalBindings,
            Permission::RetireApprovalBindings,
            Permission::ReadIntents,
            Permission::AcknowledgeIntents,
        ],
    }
}
pub(super) async fn reopen(f: Fixture) -> Fixture {
    f.reopen().await
}
pub(super) async fn fixture() -> (
    Fixture,
    String,
    hagency_core::approvals::ApprovalRoomObservation,
) {
    let f = Fixture::new().await;
    let resource_id = f.state.lock().unwrap().grant.as_ref().unwrap().resource_id.clone();
    let id = resource_id.clone();
    let room = f.registry.with_repository_for_tests(move |db| {
        let mut resource = db.resource_configuration(&id)?;
        // Current enrollment creates a private resource. This fixture exercises
        // the published fleet approval path and must explicitly publish it.
        resource.published = true;
        db.put_resource(&resource)?;
        db.register(&legacy::registration())?;
        let proof = legacy::proof(&legacy::request(
            "binding_request", "binding_agent", &resource, 20,
        ));
        let engagement = db.admit(&proof, 1000)?;
        db.approve("binding_approve", &proof, 1000)?;
        let effect = db.claim_effect()?.unwrap();
        db.observe_effect(
            &effect.id, effect.fence,
            &hagency_store::EffectOutcome::Applied { receipt: "fixture provisioning".into() },
        )?;
        let room = hagency_core::approvals::ApprovalRoomObservation {
            engagement_id: engagement.id,
            registration_generation: 1,
            generation: 1,
            room_id: "!private:example.test".into(),
            device_id: "BOT_DEVICE".into(),
            joined: std::collections::BTreeSet::from([
                "@owner:example.test".into(), "@approval:example.test".into(),
            ]),
            invite_only: true,
            encrypted: true,
            available: true,
        };
        db.observe_approval_room(&room, 1010)?;
        Ok(room)
    }).await.unwrap();
    (reopen(f).await, resource_id, room)
}
fn query(id: &str) -> ResourceApprovalBindingQuery {
    ResourceApprovalBindingQuery {
        resource_id: id.into(),
        after: String::new(),
        ..Default::default()
    }
}

#[tokio::test]
async fn native_approval_binding_receipt_failure_rolls_back_the_current_store() {
    let (f, id, _) = fixture().await;
    let m = f.owner.management(package()).await.unwrap();
    let before = m.resource_approval_bindings(query(&id)).await.unwrap().bindings.remove(0);
    let input = RetireResourceApprovalBinding {
        command_id: "receipt_failure".into(), resource_id: id.clone(),
        engagement_id: before.engagement_id.clone(), expected_revision: before.revision.clone(),
    };
    let (ctx, session) = m.authorized().await.unwrap();
    let sql = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_binding_receipt BEFORE INSERT ON management_commands WHEN NEW.operation='approval_binding.retire' BEGIN SELECT RAISE(ABORT,'fixture receipt failure'); END;").unwrap();
    assert!(session.retire_resource_approval_binding(&ctx, input.clone()).await.is_err());
    assert_eq!(m.resource_approval_bindings(query(&id)).await.unwrap().bindings[0], before);
    assert_eq!(sql.query_row("SELECT count(*) FROM management_commands WHERE operation='approval_binding.retire'", [], |r| r.get::<_, u64>(0)).unwrap(), 0);
    sql.execute_batch("DROP TRIGGER fail_binding_receipt").unwrap();
    session.retire_resource_approval_binding(&ctx, input).await.unwrap();
    assert!(m.resource_approval_bindings(query(&id)).await.unwrap().bindings.is_empty());
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
#[tokio::test]
async fn native_approval_binding_lost_reply_restart_and_successor_are_exact() {
    let (mut f, id, room) = fixture().await;
    let m = f.owner.management(package()).await.unwrap();
    let row = m
        .resource_approval_bindings(query(&id))
        .await
        .unwrap()
        .bindings
        .remove(0);
    let input = RetireResourceApprovalBinding {
        command_id: "original_unbind".into(),
        resource_id: id.clone(),
        engagement_id: row.engagement_id,
        expected_revision: row.revision,
    };
    let (ctx, session) = m.authorized().await.unwrap();
    session
        .prepare_intent(
            &ctx,
            Intent::new(
                IntentOperation::RetireApprovalBinding,
                input.command_id.clone(),
                &input,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let original = session
        .retire_resource_approval_binding(&ctx, input.clone())
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
    f = reopen(f).await;
    let m = f.owner.management(package()).await.unwrap();
    let reply = m
        .retry_saved_command(input.command_id.clone())
        .await
        .unwrap();
    assert_eq!(reply["operation"], "retire_approval_binding");
    assert_eq!(reply["data"], serde_json::to_value(&original).unwrap());
    drop(m);
    let successor = room.clone();
    f.registry.with_repository_for_tests(move |db| db.observe_approval_room(&successor, 1020)).await.unwrap();
    f = reopen(f).await;
    let m = f.owner.management(package()).await.unwrap();
    assert_eq!(
        m.retry_saved_command(input.command_id.clone())
            .await
            .unwrap()["data"],
        serde_json::to_value(&original).unwrap()
    );
    let current = m.resource_approval_bindings(query(&id)).await.unwrap();
    assert_eq!(current.bindings.len(), 1);
    assert_eq!(current.bindings[0].incarnation, 2);
    let mut reader = package();
    reader.instance += 1;
    reader
        .permissions
        .retain(|p| *p != Permission::RetireApprovalBindings);
    let reader = f.owner.management(reader).await.unwrap();
    assert!(
        reader
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
    assert_eq!(
        m.retire_resource_approval_binding(input).await.unwrap(),
        original
    );
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
#[tokio::test]
async fn native_approval_binding_conflict_is_known_only_before_original_attempt() {
    let (f, id, _) = fixture().await;
    let m = f.owner.management(package()).await.unwrap();
    let row = m
        .resource_approval_bindings(query(&id))
        .await
        .unwrap()
        .bindings
        .remove(0);
    let mut input = RetireResourceApprovalBinding {
        command_id: "stale_unbind".into(),
        resource_id: id.clone(),
        engagement_id: row.engagement_id,
        expected_revision: "b".repeat(64),
    };
    assert_eq!(
        m.retire_resource_approval_binding(input.clone())
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
    input.command_id = "unknown_unbind".into();
    let (ctx, session) = m.authorized().await.unwrap();
    session
        .prepare_intent(
            &ctx,
            Intent::new(
                IntentOperation::RetireApprovalBinding,
                input.command_id.clone(),
                &input,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        m.retire_resource_approval_binding(input.clone())
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
    assert_eq!(
        m.resource_approval_bindings(query(&id))
            .await
            .unwrap()
            .bindings
            .len(),
        1
    );
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
