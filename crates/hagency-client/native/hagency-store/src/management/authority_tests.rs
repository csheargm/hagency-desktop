use super::*;
use crate::ResourcePublicationAccess;
use std::future::Future;

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
#[test]
fn weak_authority_does_not_keep_scope_alive_and_drop_retires_resource_fence() {
    let until = Instant::now() + Duration::from_secs(30);
    let authority =
        ManagementAuthority::new(context(), [Permission::ReadResources], until).unwrap();
    let weak = authority.downgrade();
    let pending_access = ResourcePublicationAccess::new(until, authority.scope.fence.clone());
    assert!(weak.is_live());
    drop(authority);
    assert!(!weak.is_live());
    weak.retire();
    assert!(matches!(
        pending_access.prepare("resource".into(), "a".repeat(64), true, until),
        Err(Error::LocalAuthority)
    ));
}
#[test]
fn weak_authority_retires_all_strong_copies_without_replacing_original_deadline() {
    let until = Instant::now() + Duration::from_secs(30);
    let authority =
        ManagementAuthority::new(context(), [Permission::ReadResources], until).unwrap();
    let old = authority.clone();
    let weak = authority.downgrade();
    drop(authority);
    assert!(weak.is_live());
    assert_eq!(old.scope.expires, until);
    weak.retire();
    assert!(!old.is_live());
    assert!(!weak.is_live());
    assert_eq!(old.scope.expires, until);
}

async fn queued(retire: bool) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("domain");
    let store = ServerManagementJournal::open(&path, &"a".repeat(64), "@owner:test").unwrap();
    let original = Session::new_server(
        store.clone(),
        context(),
        [Permission::RequestProjects],
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let weak = original.authority().downgrade();
    let held = original.clone();
    let inspect = Connection::open(path.join("server-management.sqlite3")).unwrap();
    inspect.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut pending = Box::pin(async move {
        held.prepare_intent(
            &context(),
            Intent::new(
                IntentOperation::ProjectRequest,
                "queued_original".into(),
                &serde_json::json!({"commandId":"queued_original"}),
            )
            .unwrap(),
        )
        .await
    });
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(pending.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    drop(original);
    assert!(
        weak.is_live(),
        "queued caller retains its original strong finite scope"
    );
    if retire {
        weak.retire();
    }
    inspect.execute_batch("COMMIT").unwrap();
    let result = pending.await;
    if retire {
        assert!(matches!(result, Err(Error::LocalAuthority)), "{result:?}");
    } else {
        assert!(result.unwrap().is_first_attempt());
    }
    assert_eq!(
        inspect
            .query_row("SELECT COUNT(*) FROM management_intents", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        u64::from(!retire)
    );
    store.shutdown().await.unwrap();
    assert!(
        !weak.is_live(),
        "finished caller and writer leave only weak tracking"
    );
}
#[tokio::test]
async fn weak_authority_queued_writer_keeps_original_scope_without_cache_owner() {
    queued(false).await;
}
#[tokio::test]
async fn weak_authority_queued_writer_retirement_refuses_original_effect() {
    queued(true).await;
}
