use super::*;
use crate::management::{
    ResolveStopped, ReviewStopped, StoppedDecision, StoppedListQuery, StoppedQuery,
};
fn session(backing: &CurrentManagementStore, ctx: &HostContext) -> Session {
    Session::new_current(
        backing.clone(),
        ctx.clone(),
        [
            Permission::ReadStoppedTasks,
            Permission::InspectStoppedTasks,
            Permission::ResolveStoppedTasks,
        ],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
async fn stopped(f: &Fixture) -> StoppedQuery {
    seed(f).await;
    let id = f.resource.id();
    f.registry.0.store.call(4096,move |db|{
        db.create_canonical_task("interrupted_task","original_session","Check interrupted work",1004)?;
        db.db.execute("UPDATE runner_dispatches SET task_id='interrupted_task',input=json_set(input,'$.task_id','interrupted_task') WHERE id='original_dispatch'",[])?;
        db.db.execute("UPDATE workspace_resources SET dirty=0",[])?;
        let cap=db.claim_dispatch("fixture_runner",1005,60_000,120_000,1)?.ok_or(Error::State)?;
        db.start_dispatch(&cap,1006)?;
        db.claim_dispatch("sweeper",61_006,60_000,120_000,1)?;
        let engagement=db.db.query_row("SELECT engagement_id FROM runner_sessions WHERE id='original_session'",[],|r|r.get(0))?;
        Ok(StoppedQuery{resource_id:id,engagement_id:engagement,dispatch_id:"original_dispatch".into(),entry_offset:0})
    }).await.unwrap()
}
fn review(row: &crate::management::StoppedInspection) -> ReviewStopped {
    ReviewStopped {
        command_id: "review_original".into(),
        resource_id: row.attempt.resource_id.clone(),
        engagement_id: row.attempt.engagement_id.clone(),
        dispatch_id: row.attempt.dispatch_id.clone(),
        expected_revision: row.revision.clone(),
    }
}
fn resolve(row: &crate::management::StoppedReview) -> ResolveStopped {
    ResolveStopped {
        command_id: "decision_original".into(),
        resource_id: row.inspection.attempt.resource_id.clone(),
        engagement_id: row.inspection.attempt.engagement_id.clone(),
        dispatch_id: row.inspection.attempt.dispatch_id.clone(),
        review_command_id: row.review_command_id.clone(),
        review_revision: row.inspection.revision.clone(),
        action: StoppedDecision::KeepBlocked,
        operator_note: "Operator inspected lost process and external effects".into(),
        continuation_instruction: None,
    }
}
#[tokio::test]
async fn stopped_shared_workspace_requires_every_resource_grant_including_saved_decision() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let q = stopped(&f).await;
    let foreign = shared(&f).await;
    let ctx = context(&f);
    let m = session(&backing, &ctx);
    let page = m
        .stopped_tasks(
            &ctx,
            StoppedListQuery {
                resource_id: q.resource_id.clone(),
                workspace_id: String::new(),
                after: String::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.dispatches.len(), 1);
    assert!(!page.dispatches[0].shared_resource_authority);
    assert!(!serde_json::to_string(&page).unwrap().contains(&foreign));
    assert!(matches!(
        m.stopped_inspection(&ctx, q.clone()).await,
        Err(Error::LocalAuthority)
    ));
    let id = foreign.clone();
    let profile = f.profile.digest.clone();
    f.registry.0.store.call(1,move |db|{db.db.execute("INSERT INTO owner_resource_grants(profile,resource_id,revision,active) VALUES(?1,?2,1,1)",params![profile,id])?;Ok(())}).await.unwrap();
    let row = m.stopped_inspection(&ctx, q.clone()).await.unwrap();
    assert!(row.resolvable);
    assert!(!row.can_continue);
    let reviewed = m.review_stopped(&ctx, review(&row)).await.unwrap();
    let input = resolve(&reviewed);
    let mut invalid = input.clone();
    invalid.action = StoppedDecision::Continue;
    invalid.continuation_instruction = Some("Continue".into());
    assert!(matches!(
        m.resolve_stopped(&ctx, invalid).await,
        Err(Error::StoppedNotResolvable)
    ));
    assert_eq!(
        m.resolve_stopped(&ctx, input.clone())
            .await
            .unwrap()
            .task_status,
        "blocked"
    );
    f.registry
        .0
        .store
        .call(1, move |db| {
            db.db.execute(
                "UPDATE owner_resource_grants SET active=0 WHERE resource_id=?1",
                [foreign],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        m.resolve_stopped(&ctx, input).await,
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        m.review_stopped(&ctx, review(&row)).await,
        Err(Error::LocalAuthority)
    ));
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn stopped_queued_review_rechecks_package_retirement_before_any_write() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let q = stopped(&f).await;
    let ctx = context(&f);
    let m = session(&backing, &ctx);
    let row = m.stopped_inspection(&ctx, q).await.unwrap();
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
    let pending = m.review_stopped(&ctx, review(&row));
    tokio::pin!(pending);
    tokio::select! {result=&mut pending=>panic!("must queue: {result:?}"),_=tokio::time::sleep(Duration::from_millis(20))=>{}}
    m.retire();
    release_tx.send(()).unwrap();
    assert!(pending.await.is_err());
    blocker.await.unwrap();
    f.registry
        .0
        .store
        .call(1, |db| {
            assert_eq!(
                db.db
                    .query_row("SELECT count(*) FROM outcome_inspections", [], |r| r
                        .get::<_, u64>(0))?,
                0
            );
            assert_eq!(
                db.db.query_row(
                    "SELECT count(*) FROM management_commands WHERE operation='stopped.inspect'",
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
