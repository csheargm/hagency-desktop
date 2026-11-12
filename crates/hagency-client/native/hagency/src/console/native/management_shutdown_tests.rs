use super::*;

fn fixture() -> (tempfile::TempDir, Arc<Registry>, service::ServerManagementJournal) {
    let root = tempfile::tempdir().unwrap();
    let store = service::ServerManagementJournal::open(
        &root.path().join("server"), &"a".repeat(64), "@owner:test",
    ).unwrap();
    let registry = Arc::new(Registry::default());
    *registry.store.try_lock().unwrap() = Some(ManagementStore::Server(store.clone()));
    (root, registry, store)
}

#[tokio::test]
async fn cancelled_drain_retains_original_acknowledgement_for_concurrent_callers() {
    let (root, registry, store) = fixture();
    let context = service::HostContext::new(
        "@owner:test".into(),
        1,
        "a".repeat(64),
        1,
        "device".into(),
        1,
    )
    .unwrap();
    let session = service::Session::new_server(
        store.clone(),
        context,
        [service::Permission::ReadResources],
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    registry.remember(session.clone()).unwrap();
    let mut pending = Box::pin(registry.drain());
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(pending.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(pending);

    assert!(!session.is_live());
    assert!(matches!(
        *registry.drain.try_lock().unwrap(),
        Drain::Running(_)
    ));
    assert!(registry.store.try_lock().unwrap().is_some());
    assert!(matches!(
        service::ServerManagementJournal::open(&root.path().join("server"), &"a".repeat(64), "@owner:test"),
        Err(hagency_store::Error::Locked)
    ));
    let (one, two) = tokio::join!(registry.drain(), registry.drain());
    one.unwrap();
    two.unwrap();
    registry.drain().await.unwrap();
    assert!(!store.is_live());
    assert!(registry.store.try_lock().unwrap().is_none());
    assert_eq!(
        registry.ensure_live().unwrap_err().code,
        "management_access_required"
    );
    // Check the actual ownership lock, rather than treating channel closure as
    // proof that a new resource writer could acquire the source.
    service::ServerManagementJournal::open(&root.path().join("server"), &"a".repeat(64), "@owner:test").unwrap();
}

#[tokio::test]
async fn closed_writer_never_upgrades_a_failed_drain_to_acknowledged() {
    let (_root, registry, store) = fixture();
    store.shutdown().await.unwrap();
    assert!(!store.is_live());
    let first = registry.drain().await.unwrap_err();
    let second = registry.drain().await.unwrap_err();
    assert_eq!(first.code, "management_drain_unavailable");
    assert_eq!(second.code, first.code);
    assert!(registry.store.try_lock().unwrap().is_some());
    assert!(matches!(
        *registry.drain.try_lock().unwrap(),
        Drain::Finished(Err(DrainFailure::Unavailable))
    ));
}

#[tokio::test]
async fn empty_registry_drain_irreversibly_prevents_a_new_management_writer() {
    let registry = Registry::default();
    registry.ensure_live().unwrap();
    registry.drain().await.unwrap();
    registry.drain().await.unwrap();
    assert!(registry.store.try_lock().unwrap().is_none());
    assert_eq!(
        registry.ensure_live().unwrap_err().code,
        "management_access_required"
    );
}
