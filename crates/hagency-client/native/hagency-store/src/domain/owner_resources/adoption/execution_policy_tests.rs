use super::*;
use crate::management::{
    ExecutionPolicyListQuery, ExecutionPolicyQuery, ExecutionPolicyValue, EditExecutionPolicy,
    HostContext, Permission, Session,
};

fn context(f: &Fixture) -> HostContext {
    HostContext::new(
        f.profile.account.clone(),
        1,
        "a".repeat(64),
        1,
        "policy_device".into(),
        1,
    )
    .unwrap()
}
fn session(backing: &CurrentManagementStore, ctx: &HostContext) -> Session {
    Session::new_current(
        backing.clone(),
        ctx.clone(),
        [
            Permission::ReadExecutionPolicy,
            Permission::EditExecutionPolicy,
        ],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
async fn selection(
    f: &Fixture,
    m: &Session,
    ctx: &HostContext,
) -> (ExecutionPolicyQuery, EditExecutionPolicy) {
    approval_binding_tests::seed(f).await;
    let page = m
        .agent_execution_policies(
            ctx,
            ExecutionPolicyListQuery {
                resource_id: f.resource.id(),
                after: String::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.agents.len(), 1);
    let row = &page.agents[0];
    (
        ExecutionPolicyQuery {
            resource_id: row.resource_id.clone(),
            engagement_id: row.engagement_id.clone(),
        },
        EditExecutionPolicy {
            web_search: None,
            command_id: "policy_fixture".into(),
            resource_id: row.resource_id.clone(),
            engagement_id: row.engagement_id.clone(),
            expected_revision: row.revision.clone(),
            execution_policy: ExecutionPolicyValue { yolo: true },
        },
    )
}

#[tokio::test]
async fn adopted_execution_policy_checks_current_resource_authority_even_for_receipt() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let m = session(&backing, &ctx);
    let (query, input) = selection(&f, &m, &ctx).await;
    let result = m.edit_execution_policy(&ctx, input.clone()).await.unwrap();
    assert!(result.execution_policy.yolo);
    let mut wrong = query.clone();
    wrong.resource_id = "foreign_resource".into();
    assert!(matches!(
        m.agent_execution_policy(&ctx, wrong).await,
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
        m.edit_execution_policy(&ctx, input).await,
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        m.agent_execution_policy(&ctx, query).await,
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        m.agent_execution_policies(
            &ctx,
            ExecutionPolicyListQuery {
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
async fn adopted_execution_policy_queued_edit_rechecks_package_before_write() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let m = session(&backing, &ctx);
    let (query, input) = selection(&f, &m, &ctx).await;
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
    let pending = m.edit_execution_policy(&ctx, input);
    tokio::pin!(pending);
    tokio::select! {result=&mut pending=>panic!("write must queue: {result:?}"),_=tokio::time::sleep(Duration::from_millis(20))=>{}}
    m.retire();
    release_tx.send(()).unwrap();
    assert!(pending.await.is_err());
    blocker.await.unwrap();
    let current = session(&backing, &ctx);
    assert!(
        !current
            .agent_execution_policy(&ctx, query)
            .await
            .unwrap()
            .execution_policy
            .yolo
    );
    f.registry.0.store.call(1,|db|{assert_eq!(db.db.query_row("SELECT count(*) FROM management_commands WHERE operation='execution_policy.edit'",[],|r|r.get::<_,u64>(0))?,0);Ok(())}).await.unwrap();
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
