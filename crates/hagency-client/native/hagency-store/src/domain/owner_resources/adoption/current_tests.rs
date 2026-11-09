use super::*;
use crate::management::{HostContext, OperatorTaskQuery, Permission, ResourceQuery, Session};

fn context() -> HostContext {
    HostContext::new(
        "@owner:test".into(),
        1,
        "b".repeat(64),
        1,
        "device".into(),
        1,
    )
    .unwrap()
}
fn authority(registry: &OwnerResourceRegistry) -> ProfileInitializationAuthority {
    let until = Instant::now() + Duration::from_secs(30);
    ProfileInitializationAuthority::new(
        registry.clone(),
        ProfileAuthority::new("a".repeat(64), "@owner:test".into(), until).unwrap(),
        until,
        || true,
    )
    .unwrap()
}
fn session(store: &TestCurrentManagement) -> Session {
    Session::new_current(
        store.backing.clone(),
        context(),
        [
            Permission::ReadResources,
            Permission::ReadTasks,
            Permission::CreateTasks,
        ],
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap()
}
#[tokio::test]
async fn current_profile_initialization_reopens_without_source_provider_or_quota() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile");
    let current = TestCurrentManagement::open(&path, "@owner:test").await;
    let access = session(&current);
    assert!(
        access
            .resources(&context(), ResourceQuery::default())
            .await
            .unwrap()
            .resources
            .is_empty()
    );
    assert!(
        access
            .operator_task_page(&context(), OperatorTaskQuery::default())
            .await
            .unwrap()
            .supported
    );
    assert!(access.operator_task_write_access(&context()).await.unwrap());
    let counts=current.call(1024,|db|Ok(db.db.query_row(
        "SELECT (SELECT count(*) FROM owner_resource_adoptions),(SELECT count(*) FROM owner_operator_task_histories),(SELECT count(*) FROM resources),(SELECT count(*) FROM seats),(SELECT count(*) FROM owner_resource_grants),(SELECT count(*) FROM managed_accounts)",
        [],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,u64>(1)?,r.get::<_,u64>(2)?,r.get::<_,u64>(3)?,r.get::<_,u64>(4)?,r.get::<_,u64>(5)?)))?)).await.unwrap();
    assert_eq!(counts, (1, 1, 0, 0, 0, 0));
    assert!(!path.join("server/domain.sqlite3").exists());
    access.retire();
    current.shutdown().await.unwrap();
    drop(current);
    let reopened = TestCurrentManagement::open(&path, "@owner:test").await;
    assert!(
        session(&reopened)
            .operator_task_write_access(&context())
            .await
            .unwrap()
    );
    let count = reopened
        .call(128, |db| {
            Ok(db
                .db
                .query_row("SELECT count(*) FROM owner_resource_adoptions", [], |r| {
                    r.get::<_, u64>(0)
                })?)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert!(!path.join("server/domain.sqlite3").exists());
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn current_profile_initialization_requires_matching_journal_owner_and_profile() {
    let root = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&root.path().join("canonical")).unwrap();
    let journal =
        ServerManagementJournal::open(&root.path().join("wrong"), &"c".repeat(64), "@owner:test")
            .unwrap();
    assert!(matches!(
        registry
            .initialize_current_profile(
                authority(&registry),
                "current_test".into(),
                journal.clone()
            )
            .await,
        Err(Error::LocalAuthority)
    ));
    journal.shutdown().await.unwrap();
    registry.shutdown().await.unwrap();
}
