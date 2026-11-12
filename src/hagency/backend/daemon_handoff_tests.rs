//! Real registry locks and native drains, with no live account or database.
use super::*;
use ::hagency::native_owner::CurrentProfileSelection;
use std::{fs::File, os::unix::fs::{OpenOptionsExt, PermissionsExt}, time::Duration};

fn selection(f: &Fixture) -> (CurrentProfileSelection, File) {
    let root = f.service.root().unwrap();
    let state = if root.exists() {
        root.canonicalize().unwrap()
    } else {
        ::hagency::owner_host::initialize(root).unwrap()
    };
    let registry_root = f._directory.0.join("canonical");
    let registry = ::hagency::test_support::OwnerResourceRegistry::isolated_for_tests(&registry_root).unwrap();
    let descriptor = json!({
        "version": 4, "currentProfile": true, "registryId": registry.id(), "migrationId": "fixture-initialization",
        "origin": f.service.0.origin, "owner": f.service.0.owner,
        "profile": "a".repeat(64), "originalDevice": "fixture-device",
    });
    let mut file = File::options().write(true).create_new(true).mode(0o600)
        .open(state.join("adopted-management-initializer-v1.json")).unwrap();
    std::io::Write::write_all(&mut file, descriptor.to_string().as_bytes()).unwrap();
    let selected = CurrentProfileSelection::load_for_isolated_tests(
        &state, &f.service.0.origin, &f.service.0.owner, registry,
    ).unwrap().unwrap();
    let lock = File::options().read(true).write(true).open(registry_root.join("domain.lock")).unwrap();
    assert!(lock.try_lock().is_err(), "selection must retain the real registry writer");
    (selected, lock)
}

async fn assert_released(f: &Fixture, lock: &File) {
    assert!(f.service.0.native.lock().await.is_none());
    assert!(f.service.0.native_retirement.lock().unwrap().is_none());
    assert!(f.service.0.current_initialization.lock().unwrap().is_none());
    assert!(matches!(*f.service.0.activation.lock().await, adoption::ActivationTask::Empty));
    assert!(!f.service.0.setup_pending.load(Ordering::Acquire));
    // The registry writer exits after its last channel sender is dropped.
    tokio::time::timeout(Duration::from_secs(3), async {
        while lock.try_lock().is_err() { tokio::time::sleep(Duration::from_millis(10)).await; }
    }).await.expect("daemon must be able to acquire the canonical registry lock");
    lock.unlock().unwrap();
}

#[tokio::test]
async fn daemon_handoff_releases_cached_selection_without_native_owner() {
    let f = Fixture::new().await;
    let (selected, lock) = selection(&f);
    *f.service.0.current_initialization.lock().unwrap() = Some(selected.clone());
    *f.service.0.activation.lock().await = adoption::ActivationTask::Complete(Ok(selected));
    f.service.0.setup_pending.store(true, Ordering::Release);
    f.service.release_local_for_daemon().await.unwrap();
    assert_released(&f, &lock).await;
    f.service.release_local_for_daemon().await.unwrap();
    f.stop().await;
}

#[tokio::test]
async fn daemon_handoff_waits_for_activation_without_holding_its_native_lock() {
    let f = Fixture::new().await;
    let owner = f.open(Source::normal(&f.service)).await.unwrap();
    *f.service.0.native.lock().await = Some(owner);
    let (selected, lock) = selection(&f);
    *f.service.0.current_initialization.lock().unwrap() = Some(selected.clone());
    let release = Arc::new(tokio::sync::Notify::new());
    let resume = release.clone();
    let service = f.service.clone();
    *f.service.0.activation.lock().await = adoption::ActivationTask::Running(tokio::spawn(async move {
        resume.notified().await;
        // Activation takes this lock after the handoff has begun waiting.
        let _native = service.0.native.lock().await;
        *service.0.current_initialization.lock().unwrap() = Some(selected.clone());
        Ok(selected)
    }));
    let mut first = Box::pin(f.service.release_local_for_daemon());
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut first).await.is_err());
    drop(first);
    assert!(matches!(*f.service.0.activation.lock().await, adoption::ActivationTask::Running(_)));
    assert!(f.service.0.native_retirement.lock().unwrap().is_some());
    assert!(lock.try_lock().is_err());
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), f.service.release_local_for_daemon()).await.unwrap().unwrap();
    assert_released(&f, &lock).await;
    assert_eq!(f.revocations.load(Ordering::Acquire), 0, "ordinary handoff preserves login");
    f.stop().await;
}

#[tokio::test]
async fn daemon_handoff_retains_registry_until_cancelled_native_drain_finishes() {
    let f = Fixture::new().await;
    let owner = f.open(Source::normal(&f.service)).await.unwrap();
    *f.service.0.native.lock().await = Some(owner);
    let (selected, lock) = selection(&f);
    *f.service.0.current_initialization.lock().unwrap() = Some(selected.clone());
    *f.service.0.activation.lock().await = adoption::ActivationTask::Complete(Ok(selected));
    let barrier = Arc::new(RevokeBarrier { entered: tokio::sync::Notify::new(), release: tokio::sync::Notify::new() });
    *f.revoke_barrier.lock().unwrap() = Some(barrier.clone());
    f.service.revoke_resumption().unwrap();
    let mut first = Box::pin(f.service.release_local_for_daemon());
    tokio::select! {
        biased;
        _ = &mut first => panic!("handoff completed before native drain"),
        _ = barrier.entered.notified() => {}
    }
    drop(first);
    assert!(f.service.0.native.lock().await.is_some());
    assert!(f.service.0.native_retirement.lock().unwrap().is_some());
    assert!(f.service.0.current_initialization.lock().unwrap().is_some());
    assert!(lock.try_lock().is_err());
    barrier.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), f.service.release_local_for_daemon()).await.unwrap().unwrap();
    assert_released(&f, &lock).await;
    f.stop().await;
}

#[tokio::test]
async fn daemon_assignment_fences_cached_selection_and_local_owner_reopening() {
    let f = Fixture::new().await;
    let (selected, lock) = selection(&f);
    *f.service.0.current_initialization.lock().unwrap() = Some(selected);
    assert!(f.service.load_current_initialization().await.unwrap().is_some());
    let native = f.service.0.native.lock().await;
    let mut reopening = Box::pin(f.service.native_owner());
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut reopening).await.is_err());
    let state = f.service.root().unwrap();
    ::hagency::owner_roots::OwnerRoots::at(f._directory.0.join("resources")).unwrap()
        .resolve(&f.service.0.origin, &f.service.0.owner, Some(state)).unwrap();
    std::fs::create_dir(f.service.0.locations.daemon_home()).unwrap();
    std::fs::set_permissions(f.service.0.locations.daemon_home(), std::fs::Permissions::from_mode(0o700)).unwrap();
    ::hagency::daemon::enrollment::Enrollment::enroll(
        f.service.0.locations.daemon_home(), &f.service.0.origin, &f.service.0.owner, None, state,
    ).unwrap();
    assert_eq!(f.service.load_current_initialization().await.err().unwrap(), "owner_daemon_management_required");
    drop(native);
    assert_eq!(reopening.await.err().unwrap(), "owner_daemon_management_required");
    f.service.release_local_for_daemon().await.unwrap();
    assert_released(&f, &lock).await;
    // There is intentionally no running daemon or global SDK in this fixture.
    f.task.abort();
}
