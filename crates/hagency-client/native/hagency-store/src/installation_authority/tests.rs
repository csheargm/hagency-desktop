use super::*;
use hagency_platform::legacy_writer::ExclusiveWriterGuard;

fn until() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn shared(root: &Path) -> Result<LegacyResourceLease, Error> {
    LegacyResourceLease::at(root, until(), Arc::new(|| true))
}
fn exclusive(root: &Path) -> Result<ExclusiveWriterGuard, Error> {
    ExclusiveWriterGuard::isolated_for_tests(root).map_err(Into::into)
}

#[test]
fn retired_queued_lease_retains_exclusion_until_last_copy_is_dropped() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("registry");
    let first = shared(&root).unwrap();
    let other_profile = shared(&root).unwrap();
    let queued = first.clone();
    assert!(matches!(exclusive(&root), Err(Error::Locked)));
    first.retire();
    assert!(matches!(queued.check(), Err(Error::LocalAuthority)));
    drop(first);
    drop(other_profile);
    assert!(matches!(exclusive(&root), Err(Error::Locked)));
    drop(queued);
    let guard = exclusive(&root).unwrap();
    guard.check().unwrap();
    assert!(matches!(shared(&root), Err(Error::Locked)));
    drop(guard);
    shared(&root).unwrap().check().unwrap();
    assert!(
        !root.join("domain.sqlite3").exists(),
        "No resource ledger initialized"
    );
    assert!(
        !root.join("legacy-resource-refusal.json").exists(),
        "Writer custody must not publish migration records"
    );
}

#[test]
fn lease_subprocess() {
    let Some(root) = std::env::var_os("HAGENCY_RESOURCE_LEASE_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    assert!(matches!(shared(&root), Err(Error::Locked)));
    assert!(matches!(exclusive(&root), Err(Error::Locked)));
}

#[test]
fn exclusive_lock_denies_a_second_actual_process_and_reopens_after_drop() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("registry");
    let guard = exclusive(&root).unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "installation_authority::tests::lease_subprocess",
            "--exact",
            "--nocapture",
        ])
        .env("HAGENCY_RESOURCE_LEASE_FIXTURE", &root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    drop(guard);
    shared(&root).unwrap().check().unwrap();
}

#[test]
fn finite_native_gates_retire_without_releasing_a_held_lock() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("registry");
    private::directory(&root).unwrap();
    private::write_new(
        &root.join("legacy-resource-refusal.json"),
        b"obsolete migration state cannot grant native permission",
    )
    .unwrap();
    let live = Arc::new(AtomicBool::new(true));
    let gate = live.clone();
    let lease = LegacyResourceLease::at(
        &root,
        until(),
        Arc::new(move || gate.load(Ordering::Acquire)),
    )
    .unwrap();
    live.store(false, Ordering::Release);
    assert!(matches!(lease.check(), Err(Error::LocalAuthority)));
    assert!(matches!(exclusive(&root), Err(Error::Locked)));
    drop(lease);
    assert!(matches!(
        LegacyResourceLease::at(&root, Instant::now(), Arc::new(|| true)),
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        LegacyResourceLease::at(&root, until(), Arc::new(|| false)),
        Err(Error::LocalAuthority)
    ));
    shared(&root).unwrap().check().unwrap();
}

#[test]
fn replaced_lock_is_not_accepted_as_retained_exclusion() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("registry");
    let guard = exclusive(&root).unwrap();
    std::fs::rename(root.join(LOCK), root.join("original.lock")).unwrap();
    private::write_new(&root.join(LOCK), b"").unwrap();
    assert!(matches!(
        guard.check(),
        Err(hagency_platform::legacy_writer::Error::Private)
    ));
}
