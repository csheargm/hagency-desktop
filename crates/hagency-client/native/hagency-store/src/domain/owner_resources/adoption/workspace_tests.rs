use super::*;
use crate::management::{
    ClearWorkspaceDirty, HostContext, Permission, Session, WorkspaceListQuery, WorkspaceQuery,
    WorkspaceReleaseBlock, WorkspaceStatus,
};
use crate::domain_worker::clock_fixtures as legacy;
use hagency_core::tasks::{DispatchInput, ResourceLease, SessionBinding};

fn context(f: &Fixture) -> HostContext {
    HostContext::new(
        f.profile.account.clone(),
        1,
        "a".repeat(64),
        1,
        "workspace_device".into(),
        1,
    )
    .unwrap()
}
fn session(backing: &CurrentManagementStore, ctx: &HostContext) -> Session {
    Session::new_current(
        backing.clone(),
        ctx.clone(),
        [Permission::ReadWorkspaces, Permission::ClearWorkspaceDirty],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
fn query(f: &Fixture) -> WorkspaceQuery {
    WorkspaceQuery {
        resource_id: f.resource.id(),
        workspace_id: "shared_workspace".into(),
    }
}
fn clear(row: &WorkspaceStatus, id: &str) -> ClearWorkspaceDirty {
    ClearWorkspaceDirty {
        command_id: id.into(),
        resource_id: row.resource_id.clone(),
        workspace_id: row.workspace_id.clone(),
        expected_revision: row.revision.clone(),
    }
}
async fn seed(f: &Fixture) -> String {
    approval_binding_tests::seed(f).await;
    let resource = f.resource.clone();
    f.registry
        .0
        .store
        .call(4096, move |db| {
            let engagement: String = db.db.query_row(
                "SELECT id FROM engagements WHERE resource_id=?1",
                [resource.id()],
                |r| r.get(0),
            )?;
            db.register_workspace("shared_workspace")?;
            let binding = SessionBinding {
                id: "original_session".into(),
                engagement_id: engagement,
                room_id: "!project:example.test".into(),
                thread_root: None,
            };
            db.register_session(&binding)?;
            db.enqueue_dispatch(&DispatchInput {
                id: "original_dispatch".into(),
                session_id: binding.id,
                task_id: None,
                resources: vec![ResourceLease {
                    id: "shared_workspace".into(),
                    exclusive: true,
                }],
                payload: json!({"instruction":"fixture"}),
            })?;
            db.db
                .execute("UPDATE workspace_resources SET dirty=1", [])?;
            Ok(resource.id())
        })
        .await
        .unwrap()
}
async fn shared(f: &Fixture) -> String {
    let mut resource = f.resource.clone();
    resource.preset_id = "foreign_private_name".into();
    f.registry.0.store.call(4096,move |db|{
        db.put_resource(&resource)?;
        let proof=legacy::proof(&legacy::request("foreign_request","foreign_agent",&resource,20));
        let engagement=db.admit(&proof,1000)?;db.approve("foreign_approve",&proof,1000)?;
        let effect=db.claim_effect()?.unwrap();db.observe_effect(&effect.id,effect.fence,&crate::EffectOutcome::Applied{receipt:"fixture provisioning".into()})?;
        let binding=SessionBinding{id:"foreign_session".into(),engagement_id:engagement.id,room_id:"!project:example.test".into(),thread_root:None};
        db.register_session(&binding)?;
        // The historical association is intentionally seeded while dirty. No
        // production enqueue bypass is introduced by this isolation fixture.
        db.db.execute("INSERT INTO runner_dispatches(id,session_id,input,digest,state) VALUES('foreign_dispatch','foreign_session','{}','fixture','completed')",[])?;
        db.db.execute("INSERT INTO dispatch_resources(dispatch_id,resource_id,exclusive) VALUES('foreign_dispatch','shared_workspace',1)",[])?;
        Ok(resource.id())
    }).await.unwrap()
}

#[tokio::test]
async fn adopted_workspace_release_requires_all_shared_resource_grants_even_for_receipt() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    seed(&f).await;
    let foreign = shared(&f).await;
    let ctx = context(&f);
    let m = session(&backing, &ctx);
    let blocked = m.resource_workspace(&ctx, query(&f)).await.unwrap();
    assert_eq!(blocked.shared_resource_count, 2);
    assert_eq!(
        blocked.release_block,
        Some(WorkspaceReleaseBlock::SharedResourceAuthority)
    );
    assert!(!serde_json::to_string(&blocked).unwrap().contains(&foreign));
    assert!(matches!(
        m.clear_workspace_dirty(&ctx, clear(&blocked, "foreign_denied"))
            .await,
        Err(Error::LocalAuthority)
    ));
    let id = foreign.clone();
    let profile = f.profile.digest.clone();
    f.registry.0.store.call(1,move |db|{
        db.db.execute("INSERT INTO owner_resource_grants(profile,resource_id,revision,active) VALUES(?1,?2,1,1)",params![profile,id])?;Ok(())
    }).await.unwrap();
    let allowed = m.resource_workspace(&ctx, query(&f)).await.unwrap();
    assert_eq!(allowed.release_block, None);
    let input = clear(&allowed, "shared_release");
    assert!(
        m.clear_workspace_dirty(&ctx, input.clone())
            .await
            .unwrap()
            .cleared
    );
    let id = foreign.clone();
    f.registry
        .0
        .store
        .call(1, move |db| {
            db.db.execute(
                "UPDATE owner_resource_grants SET active=0 WHERE resource_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        m.clear_workspace_dirty(&ctx, input).await,
        Err(Error::LocalAuthority)
    ));
    let foreign_query = WorkspaceQuery {
        resource_id: foreign,
        workspace_id: "shared_workspace".into(),
    };
    assert!(matches!(
        m.resource_workspace(&ctx, foreign_query).await,
        Err(Error::LocalAuthority)
    ));
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn adopted_workspace_read_and_saved_release_recheck_selected_resource_authority() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    seed(&f).await;
    let ctx = context(&f);
    let m = session(&backing, &ctx);
    let row = m.resource_workspace(&ctx, query(&f)).await.unwrap();
    let input = clear(&row, "original_release");
    m.clear_workspace_dirty(&ctx, input.clone()).await.unwrap();
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
        m.clear_workspace_dirty(&ctx, input).await,
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        m.resource_workspace(&ctx, query(&f)).await,
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        m.resource_workspaces(
            &ctx,
            WorkspaceListQuery {
                resource_id: f.resource.id(),
                after: String::new()
            }
        )
        .await,
        Err(Error::LocalAuthority)
    ));
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn adopted_workspace_queued_release_rechecks_retired_package_before_write() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    seed(&f).await;
    let ctx = context(&f);
    let m = session(&backing, &ctx);
    let row = m.resource_workspace(&ctx, query(&f)).await.unwrap();
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
    let pending = m.clear_workspace_dirty(&ctx, clear(&row, "queued_release"));
    tokio::pin!(pending);
    tokio::select! {result=&mut pending=>panic!("release must queue: {result:?}"),_=tokio::time::sleep(Duration::from_millis(20))=>{}}
    m.retire();
    release_tx.send(()).unwrap();
    assert!(pending.await.is_err());
    blocker.await.unwrap();
    let current = session(&backing, &ctx);
    assert_eq!(
        current.resource_workspace(&ctx, query(&f)).await.unwrap(),
        row
    );
    f.registry.0.store.call(1,|db|{
        assert_eq!(db.db.query_row("SELECT count(*) FROM management_commands WHERE operation='workspace.clear_dirty'",[],|r|r.get::<_,u64>(0))?,0);Ok(())
    }).await.unwrap();
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[path = "stopped_tests.rs"]
mod stopped_tests;
