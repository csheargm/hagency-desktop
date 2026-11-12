use super::*;
use crate::owner_resources::adoption::TestCurrentManagement;
use crate::management::{HostContext, Permission, Session, PrepareAccount, AccountInventoryQuery};
use std::time::{Instant, Duration};
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
fn session(store: TestCurrentManagement) -> Session {
    Session::new_current(
        store.backing.clone(),
        context(),
        [
            Permission::PrepareAccounts,
            Permission::ReadAccounts,
            Permission::LoginAccounts,
        ],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
async fn account(s: &Session) -> AccountLoginRequest {
    let a = s
        .prepare_account(
            &context(),
            PrepareAccount {
                command_id: "prepare-original".into(),
            },
        )
        .await
        .unwrap();
    let inventory = s
        .account_inventory(&context(), AccountInventoryQuery::default())
        .await
        .unwrap();
    AccountLoginRequest {
        command_id: "login-original".into(),
        account_id: a.account_id,
        expected_revision: inventory.accounts[0].revision.clone().unwrap(),
    }
}
fn verdict(
    outcome: crate::LoginOutcome,
    mode: crate::AccountReadinessMode,
    word: &str,
) -> crate::LoginVerdict {
    crate::LoginVerdict {
        outcome,
        mode,
        provider_state: word.into(),
        expires_at_ms: None,
    }
}
#[tokio::test]
async fn managed_login_original_effect_gap_reopen_never_respawns() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("store");
    let store = TestCurrentManagement::open(&path, "@owner:test").await;
    let s = session(store.clone());
    let input = account(&s).await;
    let original = s
        .begin_account_login(&context(), input.clone())
        .await
        .unwrap();
    assert!(original.handle.is_some());
    let replay = s
        .begin_account_login(&context(), input.clone())
        .await
        .unwrap();
    assert!(replay.handle.is_none());
    assert_eq!(replay.status.state, "uncertain");
    let changed = AccountLoginRequest {
        expected_revision: "f".repeat(64),
        ..input.clone()
    };
    assert!(s.begin_account_login(&context(), changed).await.is_err());
    drop(original);
    s.retire();
    drop(s);
    store.shutdown().await.unwrap();
    drop(store);
    let store = TestCurrentManagement::open(&path, "@owner:test").await;
    let s = session(store.clone());
    let restored = s
        .begin_account_login(&context(), input.clone())
        .await
        .unwrap();
    assert!(restored.handle.is_none());
    assert_eq!(restored.status.state, "uncertain");
    assert_eq!(
        s.account_inventory(&context(), AccountInventoryQuery::default())
            .await
            .unwrap()
            .accounts[0]
            .readiness,
        "unknown"
    );
    let counts = store
        .call(128, |db| {
            Ok((
                db.db
                    .query_row("SELECT count(*) FROM account_login_attempts", [], |r| {
                        r.get::<_, u64>(0)
                    })?,
                db.db
                    .query_row("SELECT count(*) FROM account_login_observations", [], |r| {
                        r.get::<_, u64>(0)
                    })?,
            ))
        })
        .await
        .unwrap();
    // Canonical reopen retains the original uncertain attempt without manufacturing
    // an observation or launching a replacement login.
    assert_eq!(counts, (1, 0));
    store.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_login_receipts_follow_original_attempt_without_credentials_or_resources() {
    let root = tempfile::tempdir().unwrap();
    let store = TestCurrentManagement::open(&root.path().join("state"), "@owner:test").await;
    let s = session(store.clone());
    let input = account(&s).await;
    let start = s
        .begin_account_login(&context(), input.clone())
        .await
        .unwrap();
    let mut env = std::collections::BTreeMap::new();
    start
        .handle
        .as_ref()
        .unwrap()
        .launch()
        .apply_codex_environment(&mut env)
        .unwrap();
    assert_eq!(
        env.get(std::ffi::OsStr::new("HOME")),
        Some(&hagency_platform::codex_process_home(std::path::Path::new(env.get(std::ffi::OsStr::new("CODEX_HOME")).unwrap())).unwrap().into_os_string())
    );
    start
        .handle
        .unwrap()
        .settle(verdict(
            crate::LoginOutcome::Observed,
            crate::AccountReadinessMode::Subscription,
            "signed_in",
        ))
        .await
        .unwrap();
    assert_eq!(
        s.account_login_status(&context(), input.clone())
            .await
            .unwrap()
            .state,
        "authenticated"
    );
    let second = AccountLoginRequest {
        command_id: "login-second".into(),
        ..input.clone()
    };
    let next = s
        .begin_account_login(&context(), second.clone())
        .await
        .unwrap();
    assert_eq!(s.account_inventory(&context(),AccountInventoryQuery::default()).await.unwrap().accounts[0].readiness,"unknown");
    assert_eq!(
        s.account_login_status(&context(), input.clone())
            .await
            .unwrap()
            .state,
        "superseded"
    );
    next.handle
        .unwrap()
        .settle(verdict(
            crate::LoginOutcome::Refused,
            crate::AccountReadinessMode::Unknown,
            "cancelled",
        ))
        .await
        .unwrap();
    assert_eq!(
        s.account_login_status(&context(), second)
            .await
            .unwrap()
            .state,
        "cancelled"
    );
    assert_eq!(
        s.account_inventory(&context(), AccountInventoryQuery::default())
            .await
            .unwrap()
            .accounts[0]
            .readiness,
        "unknown"
    );
    assert!(
        s.begin_account_login(&context(), input)
            .await
            .unwrap()
            .handle
            .is_none()
    );
    let count = store
        .call(128, |db| {
            Ok(db
                .db
                .query_row("SELECT count(*) FROM resources", [], |r| r.get::<_, u64>(0))?)
        })
        .await
        .unwrap();
    assert_eq!(count, 0);
    store.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_login_current_scope_and_profile_checks_precede_any_attempt() {
    let root = tempfile::tempdir().unwrap();
    let store = TestCurrentManagement::open(&root.path().join("state"), "@owner:test").await;
    let s = session(store.clone());
    let input = account(&s).await;
    let reader = Session::new_current(
        store.backing.clone(),
        context(),
        [Permission::ReadAccounts],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        reader
            .begin_account_login(&context(), input.clone())
            .await
            .is_err()
    );
    let stale = AccountLoginRequest {
        expected_revision: "b".repeat(64),
        ..input.clone()
    };
    assert!(s.begin_account_login(&context(), stale).await.is_err());
    let different = HostContext::new(
        "@other:test".into(),
        1,
        "a".repeat(64),
        1,
        "device".into(),
        1,
    )
    .unwrap();
    assert!(
        s.begin_account_login(&different, input.clone())
            .await
            .is_err()
    );
    // Canonical ownership is a stronger boundary than merely knowing the ID.
    let selected = input.clone();
    store
        .call(4096, move |db| {
            let receipt =
                CommandReceipt::new(&context(), "account.login", &selected.command_id, &selected)?;
            assert!(
                db.management_begin_account_login(&"b".repeat(64), selected, receipt, || Ok(()))
                    .is_err()
            );
            Ok(())
        })
        .await
        .unwrap();
    s.retire();
    assert!(s.begin_account_login(&context(), input).await.is_err());
    let count = store
        .call(128, |db| {
            Ok(db
                .db
                .query_row("SELECT count(*) FROM account_login_attempts", [], |r| {
                    r.get::<_, u64>(0)
                })?)
        })
        .await
        .unwrap();
    assert_eq!(count, 0);
    store.shutdown().await.unwrap();
}
#[test]
fn managed_login_receipt_failure_rolls_back_original_attempt() {
    let root = tempfile::tempdir().unwrap();
    let mut db =
        DomainRepository::open_managed(&root.path().join("state"), &"a".repeat(64)).unwrap();
    let a = db.reserve_account(crate::ACCOUNT_PROFILE).unwrap();
    let a = db.materialize_account(&a.id).unwrap();
    let result = db.begin_account_login_checked(&a.id, graphs::now_ms().unwrap(), |_, _| {
        Err(Error::Unavailable)
    });
    assert!(result.is_err());
    let count: u64 = db
        .db
        .query_row("SELECT count(*) FROM account_login_attempts", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}
