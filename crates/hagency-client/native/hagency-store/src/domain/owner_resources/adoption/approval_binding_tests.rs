use super::*;
use crate::management::{
    HostContext, Permission, ResourceApprovalBindingQuery, RetireResourceApprovalBinding, Session,
};
use crate::domain_worker::clock_fixtures as legacy;

fn context(f: &Fixture) -> HostContext {
    HostContext::new(
        f.profile.account.clone(),
        1,
        "a".repeat(64),
        1,
        "bindings_device".into(),
        1,
    )
    .unwrap()
}
fn session(backing: &CurrentManagementStore, context: &HostContext) -> Session {
    Session::new_current(
        backing.clone(),
        context.clone(),
        [
            Permission::ReadApprovalBindings,
            Permission::RetireApprovalBindings,
        ],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
fn query(f: &Fixture) -> ResourceApprovalBindingQuery {
    ResourceApprovalBindingQuery {
        resource_id: f.resource.id(),
        after: String::new(),
        ..Default::default()
    }
}
pub(super) async fn seed(f: &Fixture) {
    let resource = f.resource.clone();
    f.registry
        .0
        .store
        .call(4096, move |db| {
            db.register(&legacy::registration())?;
            let proof = legacy::proof(&legacy::request(
                "binding_request",
                "binding_agent",
                &resource,
                20,
            ));
            let engagement = db.admit(&proof, 1000)?;
            db.approve("binding_approve", &proof, 1000)?;
            let effect = db.claim_effect()?.unwrap();
            db.observe_effect(
                &effect.id,
                effect.fence,
                &crate::EffectOutcome::Applied {
                    receipt: "fixture provisioning".into(),
                },
            )?;
            db.observe_approval_room(
                &hagency_core::approvals::ApprovalRoomObservation {
                    engagement_id: engagement.id,
                    registration_generation: 1,
                    generation: 1,
                    room_id: "!private:example.test".into(),
                    device_id: "BOT_DEVICE".into(),
                    joined: std::collections::BTreeSet::from([
                        "@owner:example.test".into(),
                        "@approval:example.test".into(),
                    ]),
                    invite_only: true,
                    encrypted: true,
                    available: true,
                },
                1010,
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn adopted_approval_bindings_require_current_resource_grant_even_for_receipt() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let manager = session(&backing, &ctx);
    seed(&f).await;
    let rows = manager
        .resource_approval_bindings(&ctx, query(&f))
        .await
        .unwrap();
    assert_eq!(rows.bindings.len(), 1);
    let selected = &rows.bindings[0];
    assert!(selected.active);
    assert_eq!(selected.owner_mxid, "@owner:example.test");
    let input = RetireResourceApprovalBinding {
        command_id: "adopted_original_unbind".into(),
        resource_id: f.resource.id(),
        engagement_id: selected.engagement_id.clone(),
        expected_revision: selected.revision.clone(),
    };
    let original = manager
        .retire_resource_approval_binding(&ctx, input.clone())
        .await
        .unwrap();
    assert!(original.unbound);
    assert!(
        manager
            .resource_approval_bindings(&ctx, query(&f))
            .await
            .unwrap()
            .bindings
            .is_empty()
    );
    assert_eq!(
        manager
            .retire_resource_approval_binding(&ctx, input.clone())
            .await
            .unwrap(),
        original
    );
    let mut foreign = query(&f);
    foreign.resource_id = "other_resource".into();
    assert!(matches!(
        manager.resource_approval_bindings(&ctx, foreign).await,
        Err(Error::LocalAuthority)
    ));
    f.registry
        .0
        .store
        .call(1, |db| {
            db.db
                .execute("UPDATE owner_resource_grants SET active=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        manager.resource_approval_bindings(&ctx, query(&f)).await,
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        manager.retire_resource_approval_binding(&ctx, input).await,
        Err(Error::LocalAuthority)
    ));
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn adopted_approval_binding_queued_removal_rechecks_retired_package() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let manager = session(&backing, &ctx);
    seed(&f).await;
    let selected = manager
        .resource_approval_bindings(&ctx, query(&f))
        .await
        .unwrap()
        .bindings
        .remove(0);
    let input = RetireResourceApprovalBinding {
        command_id: "queued_unbind".into(),
        resource_id: f.resource.id(),
        engagement_id: selected.engagement_id,
        expected_revision: selected.revision,
    };
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = f.registry.0.store.clone();
    let blocker = tokio::spawn(async move {
        store
            .call(1, move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            })
            .await
            .unwrap()
    });
    entered_rx.await.unwrap();
    let pending = manager.retire_resource_approval_binding(&ctx, input);
    tokio::pin!(pending);
    tokio::select! {result=&mut pending=>panic!("write must queue: {result:?}"), _=tokio::time::sleep(Duration::from_millis(20))=>{}}
    manager.retire();
    release_tx.send(()).unwrap();
    assert!(pending.await.is_err());
    blocker.await.unwrap();
    let current = session(&backing, &ctx);
    assert_eq!(
        current
            .resource_approval_bindings(&ctx, query(&f))
            .await
            .unwrap()
            .bindings
            .len(),
        1
    );
    f.registry.0.store.call(1, |db| {
        assert_eq!(db.db.query_row("SELECT count(*) FROM management_commands WHERE operation='approval_binding.retire'", [], |r| r.get::<_,u64>(0))?, 0);
        Ok(())
    }).await.unwrap();
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
