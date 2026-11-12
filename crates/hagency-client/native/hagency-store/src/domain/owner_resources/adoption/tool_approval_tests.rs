use super::*;
use crate::management::{HostContext, Permission, Session, ToolGrantQuery, RevokeToolGrant};

fn context(f: &Fixture) -> HostContext {
    HostContext::new(
        f.profile.account.clone(),
        1,
        "a".repeat(64),
        1,
        "tools_device".into(),
        1,
    )
    .unwrap()
}
fn session(backing: &CurrentManagementStore, ctx: &HostContext) -> Session {
    Session::new_current(
        backing.clone(),
        ctx.clone(),
        [
            Permission::ReadToolApprovals,
            Permission::InspectToolGrants,
            Permission::RevokeToolGrants,
        ],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
async fn seed(f: &Fixture) -> String {
    approval_binding_tests::seed(f).await;
    // Fixture for management scope/receipt transport. The integration suite
    // separately creates real rules through request_owner_approval + verdict.
    f.registry.0.store.call(4096,|db|{
        let engagement:String=db.db.query_row("SELECT engagement_id FROM approval_bindings",[],|r|r.get(0))?;
        db.db.execute("INSERT INTO approval_grants(id,engagement_id,binding_generation,scope_key,scope_kind,mode,context_key) VALUES('grant_fixture',?1,1,'scope_fixture','\"exact_command\"','always','context_fixture')",[&engagement])?;
        Ok(engagement)
    }).await.unwrap()
}
async fn selected(
    f: &Fixture,
    m: &Session,
    ctx: &HostContext,
    engagement: &str,
) -> RevokeToolGrant {
    let row = m
        .tool_grants(
            ctx,
            ToolGrantQuery {
                resource_id: f.resource.id(),
                engagement_id: engagement.into(),
                after: String::new(),
            },
        )
        .await
        .unwrap()
        .grants
        .remove(0);
    assert!(row.details.is_none());
    RevokeToolGrant {
        command_id: "revoke_fixture".into(),
        resource_id: row.resource_id,
        engagement_id: row.engagement_id,
        grant_id: row.id,
        expected_revision: row.revision,
    }
}
#[tokio::test]
async fn adopted_tool_grants_require_resource_authority_even_for_original_receipt() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let m = session(&backing, &ctx);
    let engagement = seed(&f).await;
    let input = selected(&f, &m, &ctx, &engagement).await;
    let original = m.revoke_tool_grant(&ctx, input.clone()).await.unwrap();
    assert_eq!(
        m.revoke_tool_grant(&ctx, input.clone()).await.unwrap(),
        original
    );
    let wrong = ToolGrantQuery {
        resource_id: "foreign_resource".into(),
        engagement_id: engagement.clone(),
        after: String::new(),
    };
    assert!(matches!(
        m.tool_grants(&ctx, wrong).await,
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
        m.revoke_tool_grant(&ctx, input).await,
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        m.tool_grants(
            &ctx,
            ToolGrantQuery {
                resource_id: f.resource.id(),
                engagement_id: engagement,
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
async fn adopted_tool_grant_queued_revoke_rechecks_package() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let m = session(&backing, &ctx);
    let engagement = seed(&f).await;
    let input = selected(&f, &m, &ctx, &engagement).await;
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
    let pending = m.revoke_tool_grant(&ctx, input);
    tokio::pin!(pending);
    tokio::select! {result=&mut pending=>panic!("write must queue: {result:?}"),_=tokio::time::sleep(Duration::from_millis(20))=>{}}
    m.retire();
    release_tx.send(()).unwrap();
    assert!(pending.await.is_err());
    blocker.await.unwrap();
    let current = session(&backing, &ctx);
    assert!(
        !current
            .tool_grants(
                &ctx,
                ToolGrantQuery {
                    resource_id: f.resource.id(),
                    engagement_id: engagement,
                    after: String::new()
                }
            )
            .await
            .unwrap()
            .grants[0]
            .revoked
    );
    f.registry
        .0
        .store
        .call(1, |db| {
            assert_eq!(
                db.db.query_row(
                    "SELECT count(*) FROM management_commands WHERE operation='tool_grant.revoke'",
                    [],
                    |r| r.get::<_, u64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
