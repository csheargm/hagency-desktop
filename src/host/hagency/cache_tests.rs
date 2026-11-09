use super::*;
use crate::hagency::backend::retirement_tests::NativeFixture;

fn host_and_lease() -> (Host, Lease) {
    let authority = rinx_miniapp_core::SessionAuthority::default();
    let identity = InstanceId {
        app: octosense_app_contract::hagency::APP_ID.into(),
        account: "@owner:test".into(),
        room: None,
        generation: 1,
    };
    let lease = authority.issue(
        identity.clone(),
        ["hagency.resources.list".to_owned()].into(),
        Default::default(),
        Instant::now() + Duration::from_secs(60),
    );
    let host = Host(Arc::new(Inner {
        digest: "a".repeat(64),
        identity,
        epoch: crate::account_session::epoch(),
        retired: AtomicBool::new(false),
        reauthentication_required: AtomicBool::new(false),
        initialising: Default::default(),
        manager: Default::default(),
        grant_access: Default::default(),
    }));
    (host, lease)
}
fn package() -> ManagementPackage {
    ManagementPackage {
        digest: "a".repeat(64),
        instance: 1,
        account_epoch: 1,
        permissions: vec![ManagementPermission::ReadResources],
    }
}

#[test]
fn host_request_future_fits_backend_stack() {
    // Construct on a large test stack so a regression reports its size instead
    // of aborting the test process before the assertion can run. The real host
    // dispatch runs on Tokio's ordinary worker stack.
    let bytes = std::thread::Builder::new().stack_size(16 * 1024 * 1024).spawn(|| {
        let (host, lease) = host_and_lease();
        let request = host.request(lease, "hagency.resources.list", json!({}));
        std::mem::size_of_val(&request)
    }).unwrap().join().unwrap();
    assert!(bytes <= 128 * 1024, "host request future is {bytes} bytes");
}

#[test]
fn host_request_polls_on_backend_stack() {
    let runtime = crate::sliding_sync::backend_runtime();
    runtime.block_on(async {
        let result = runtime.spawn(async {
            let (host, lease) = host_and_lease();
            host.request(lease, "hagency.resources.list", json!({})).await
        }).await.unwrap();
        // No SDK session is installed: exercise actual host polling through its
        // first authorization check, with no account or network mutation.
        assert!(result.is_err());
    });
}

// Uses the real host cache and native OS profile lock. No global SDK account,
// CURRENT service, production profile, provider process or Matrix room changes.
#[tokio::test]
async fn retired_host_cache_releases_profile_before_fresh_native_authorization() {
    let fixture = NativeFixture::new().await;
    let owner = fixture.open_detached().await.unwrap();
    let native = owner.management(package()).await.unwrap();
    // Exercise the same facade cached by production, while tracking the
    // underlying native Arc that owns the real profile lock.
    let old = Arc::downgrade(&native);
    let manager = NativeManagement::local(native);
    assert!(!manager.is_retired());
    assert!(
        manager
            .resources(Default::default())
            .await
            .unwrap()
            .resources
            .is_empty()
    );
    let (host, lease) = host_and_lease();
    *host.0.manager.lock().unwrap() = Some(manager.clone());
    assert!(Arc::ptr_eq(
        &host.cached_manager().unwrap().unwrap(),
        &manager
    ));

    // Failed logout has retired/cleaned the native owner, while the same
    // account's still-open mini app retains its cached manager.
    owner.shutdown().await;
    assert!(manager.is_retired());
    assert!(manager.resources(Default::default()).await.is_err());
    drop(manager);
    drop(owner);
    assert!(old.upgrade().is_some());
    assert_eq!(
        fixture.open_detached().await.err().unwrap().code,
        "native_owner_state_unavailable"
    );

    // The production cache lookup drops that retired Arc before the host can
    // try to open a freshly authorized native service on the same profile.
    assert!(host.cached_manager().unwrap().is_none());
    assert!(old.upgrade().is_none());
    assert_eq!(
        fixture.open_detached().await.err().unwrap().code,
        "native_resume_revoked",
        "evicting a cached manager must not restore a logged-out session"
    );
    let fresh_owner = fixture.open_after_explicit_signin().await.unwrap();
    let fresh = NativeManagement::local(fresh_owner.management(package()).await.unwrap());
    assert!(!fresh.is_retired());
    assert!(
        fresh
            .resources(Default::default())
            .await
            .unwrap()
            .resources
            .is_empty()
    );
    *host.0.manager.lock().unwrap() = Some(fresh.clone());
    assert!(Arc::ptr_eq(
        &host.cached_manager().unwrap().unwrap(),
        &fresh
    ));

    host.retire();
    assert!(host.0.manager.lock().unwrap().is_none());
    assert!(fresh.is_retired());
    assert!(fresh.resources(Default::default()).await.is_err());
    assert_eq!(
        host.manager(&lease).await.err().unwrap(),
        "management_scope_changed"
    );
    fresh_owner.shutdown().await;
    drop(fresh);
    drop(fresh_owner);
    fixture.stop().await;
}

#[tokio::test]
async fn retired_package_or_old_epoch_cannot_recreate_a_cached_manager() {
    let (host, lease) = host_and_lease();
    host.retire();
    assert_eq!(
        host.manager(&lease).await.err().unwrap(),
        "management_scope_changed"
    );
    assert!(host.0.manager.lock().unwrap().is_none());

    let (mut old, lease) = host_and_lease();
    Arc::get_mut(&mut old.0).unwrap().epoch = u64::MAX;
    assert_eq!(
        old.manager(&lease).await.err().unwrap(),
        "management_scope_changed"
    );
    assert!(old.0.manager.lock().unwrap().is_none());
}

#[cfg(feature = "resource-adoption")]
#[tokio::test]
async fn failed_adoption_cache_eviction_releases_original_profile_before_recovery() {
    let fixture = NativeFixture::new().await;
    let owner = fixture.open_detached().await.unwrap();
    let manager = NativeManagement::local(owner.management(package()).await.unwrap());
    let (host, _) = host_and_lease();
    *host.0.manager.lock().unwrap() = Some(manager.clone());
    host.evict_retired_setup_caches().unwrap();
    assert!(host.0.manager.lock().unwrap().is_some());
    owner.shutdown().await;
    drop(manager);
    drop(owner);
    assert_eq!(
        fixture.open_detached().await.err().unwrap().code,
        "native_owner_state_unavailable"
    );
    host.evict_retired_setup_caches().unwrap();
    assert!(host.0.manager.lock().unwrap().is_none());
    assert_eq!(
        fixture.open_detached().await.err().unwrap().code,
        "native_resume_revoked"
    );
    let fresh = fixture.open_after_explicit_signin().await.unwrap();
    fresh.shutdown().await;
    drop(fresh);
    fixture.stop().await;
}
