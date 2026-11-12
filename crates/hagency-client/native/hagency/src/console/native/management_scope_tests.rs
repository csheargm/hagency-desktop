use super::*;

fn context(instance: u64) -> service::HostContext {
    service::HostContext::new(
        "@owner:test".into(),
        1,
        "a".repeat(64),
        instance,
        "device".into(),
        1,
    )
    .unwrap()
}
fn session(store: &service::ServerManagementJournal, instance: u64) -> service::Session {
    service::Session::new_server(
        store.clone(),
        context(instance),
        [service::Permission::ReadResources],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
async fn fixture() -> (tempfile::TempDir, service::ServerManagementJournal) {
    let root = tempfile::tempdir().unwrap();
    let store = service::ServerManagementJournal::open(
        &root.path().join("server"), &"a".repeat(64), "@owner:test",
    ).unwrap();
    (root, store)
}
#[tokio::test]
async fn management_weak_registry_bounds_actual_live_scopes_and_prunes_dropped_cache_entries() {
    let (_root, store) = fixture().await;
    let registry = Registry::default();
    for _ in 0..100 {
        let old = session(&store, 1);
        registry.remember(old.clone()).unwrap();
        assert_eq!(registry.live_scope_count(), 1);
        drop(old);
        assert_eq!(registry.live_scope_count(), 0);
    }
    let mut live = Vec::new();
    for index in 1..=16 {
        let scope = session(&store, index);
        registry.remember(scope.clone()).unwrap();
        live.push(scope);
    }
    assert_eq!(registry.live_scope_count(), 16);
    let denied = session(&store, 17);
    assert_eq!(
        registry.remember(denied.clone()).unwrap_err().code,
        "management_capacity"
    );
    assert!(!denied.is_live());
    drop(live.pop());
    let admitted = session(&store, 18);
    registry.remember(admitted.clone()).unwrap();
    assert_eq!(registry.live_scope_count(), 16);
    registry.retire();
    assert!(live.iter().all(|scope| !scope.is_live()));
    assert!(!admitted.is_live());
    store.shutdown().await.unwrap();
}
#[tokio::test]
async fn management_package_retirement_fences_old_inflight_scope_after_cache_replacement() {
    let (_root, store) = fixture().await;
    let old = session(&store, 1);
    let latest = session(&store, 1);
    let mut cache = PackageSessions {
        deadline:None,
        current: Some((context(1), latest.clone())),
        issued: vec![old.authority().downgrade(), latest.authority().downgrade()],
    };
    assert!(old.is_live() && latest.is_live());
    cache.retire();
    assert!(!old.is_live() && !latest.is_live());
    assert!(cache.current.is_none());
    store.shutdown().await.unwrap();
}
