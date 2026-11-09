use super::*;
use crate::owner_resources::adoption::TestCurrentManagement;
use crate::management::{
    Session, HostContext, Permission, PrepareAccount, AccountInventoryQuery, AccountLoginRequest,
};
use std::time::{Duration, Instant};
fn context() -> HostContext {
    HostContext::new(
        "@owner:test".into(),
        1,
        "a".repeat(64),
        1,
        "device".into(),
        1,
    )
    .unwrap()
}
async fn fixture() -> (
    tempfile::TempDir,
    TestCurrentManagement,
    Session,
    AccountModelQuery,
) {
    let root = tempfile::tempdir().unwrap();
    let store = TestCurrentManagement::open(&root.path().join("state"), "@owner:test").await;
    let session = Session::new_current(
        store.backing.clone(),
        context(),
        [
            Permission::PrepareAccounts,
            Permission::ReadAccounts,
            Permission::LoginAccounts,
            Permission::ReadAccountModels,
        ],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let account = session
        .prepare_account(
            &context(),
            PrepareAccount {
                command_id: "original".into(),
            },
        )
        .await
        .unwrap();
    let inventory = session
        .account_inventory(&context(), AccountInventoryQuery::default())
        .await
        .unwrap();
    let query = AccountModelQuery {
        account_id: account.account_id,
        expected_revision: inventory.accounts[0].revision.clone().unwrap(),
    };
    (root, store, session, query)
}
#[tokio::test]
async fn account_inspection_is_original_namespace_and_has_no_login_or_resource_effect() {
    let (_root, store, session, query) = fixture().await;
    let inspection = session
        .inspect_account(&context(), query.clone())
        .await
        .unwrap();
    let mut env = std::collections::BTreeMap::new();
    inspection
        .launch()
        .apply_codex_environment(&mut env)
        .unwrap();
    assert_eq!(
        env.get(std::ffi::OsStr::new("HOME")),
        Some(&hagency_platform::codex_process_home(std::path::Path::new(env.get(std::ffi::OsStr::new("CODEX_HOME")).unwrap())).unwrap().into_os_string())
    );
    assert!(
        std::path::Path::new(env.get(std::ffi::OsStr::new("CODEX_HOME")).unwrap())
            .ends_with(&query.account_id)
    );
    inspection.check().await.unwrap();
    let counts=store.call(128,|db|Ok(db.db.query_row("SELECT (SELECT count(*) FROM account_login_attempts),(SELECT count(*) FROM account_login_observations),(SELECT count(*) FROM resources)",[],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,u64>(1)?,r.get::<_,u64>(2)?)))?)).await.unwrap();
    assert_eq!(counts, (0, 0, 0));
    store.shutdown().await.unwrap();
}
#[tokio::test]
async fn account_inspection_rechecks_grant_revision_profile_and_pending_login() {
    let (_root, store, session, query) = fixture().await;
    let reader = Session::new_current(
        store.backing.clone(),
        context(),
        [Permission::ReadAccounts],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        reader
            .inspect_account(&context(), query.clone())
            .await
            .is_err()
    );
    let stale = AccountModelQuery {
        expected_revision: "f".repeat(64),
        ..query.clone()
    };
    assert!(session.inspect_account(&context(), stale).await.is_err());
    let copy = query.clone();
    assert!(
        store
            .call(128, move |db| db
                .management_inspect_account("different-profile", &copy)
                .map(|_| ()))
            .await
            .is_err()
    );
    let inspection = session
        .inspect_account(&context(), query.clone())
        .await
        .unwrap();
    let login = session
        .begin_account_login(
            &context(),
            AccountLoginRequest {
                command_id: "login-original".into(),
                account_id: query.account_id.clone(),
                expected_revision: query.expected_revision.clone(),
            },
        )
        .await
        .unwrap();
    assert!(inspection.check().await.is_err());
    assert!(
        session
            .inspect_account(&context(), query.clone())
            .await
            .is_err()
    );
    login
        .handle
        .unwrap()
        .settle(crate::LoginVerdict {
            mode: crate::AccountReadinessMode::Unknown,
            outcome: crate::LoginOutcome::Refused,
            provider_state: "cancelled".into(),
            expires_at_ms: None,
        })
        .await
        .unwrap();
    session.inspect_account(&context(), query).await.unwrap();
    session.retire();
    assert!(inspection.check().await.is_err());
    store.shutdown().await.unwrap();
}
#[tokio::test]
async fn account_inspection_rejects_retired_namespace_after_capture() {
    let (_root, store, session, query) = fixture().await;
    let inspection = session
        .inspect_account(&context(), query.clone())
        .await
        .unwrap();
    store
        .call(128, move |db| {
            db.retire_account(&query.account_id, crate::LogoutObservation::unobserved())
                .map(|_| ())
        })
        .await
        .unwrap();
    assert!(inspection.check().await.is_err());
    assert!(inspection.launch().check().is_err());
    store.shutdown().await.unwrap();
}
