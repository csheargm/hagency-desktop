use super::*;

#[test]
fn shared_lifetime_custody_blocks_exclusive_until_last_connection_guard_drops() {
    const CHILD: &str = "HAGENCY_SHARED_LIFETIME_CUSTODY_CHILD";
    if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("1")) {
        // Create the guards only after exec, with no sibling tests running.
        // A parallel test can fork while these guards are held; CLOEXEC does
        // not release that child's inherited locks until it reaches exec.
        let name = concat!(
            module_path!(),
            "::shared_lifetime_custody_blocks_exclusive_until_last_connection_guard_drops"
        )
        .strip_prefix("hagency_platform::")
        .unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(
            result.status.success() && stdout.contains("test result: ok. 1 passed;"),
            "isolated custody assertion did not pass ({:?}):\n{stdout}\n{}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("authority");
    let a = LegacyWriterGuard::isolated_for_tests(&root).unwrap();
    let b = LegacyWriterGuard::isolated_for_tests(&root).unwrap();
    assert!(matches!(
        ExclusiveWriterGuard::isolated_for_tests(&root),
        Err(Error::Locked)
    ));
    drop(a);
    assert!(matches!(
        ExclusiveWriterGuard::isolated_for_tests(&root),
        Err(Error::Locked)
    ));
    drop(b);
    let exclusive = ExclusiveWriterGuard::isolated_for_tests(&root).unwrap();
    assert!(matches!(
        LegacyWriterGuard::isolated_for_tests(&root),
        Err(Error::Locked)
    ));
    exclusive.check().unwrap();
    assert!(!root.join("legacy-resource-refusal.json").exists());
}
#[test]
fn guarded_process_child() {
    let Some(root) = std::env::var_os("HAGENCY_GUARD_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    assert!(matches!(
        ExclusiveWriterGuard::isolated_for_tests(&root),
        Err(Error::Locked)
    ));
    LegacyWriterGuard::isolated_for_tests(&root).unwrap();
}
#[test]
fn lifetime_shared_guard_excludes_an_actual_second_process() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("authority");
    let _guard = LegacyWriterGuard::isolated_for_tests(&root).unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["legacy_writer::tests::guarded_process_child", "--exact"])
        .env("HAGENCY_GUARD_TEST_ROOT", &root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
#[test]
fn obsolete_migration_records_do_not_control_current_writer_custody() {
    const CHILD: &str = "HAGENCY_OBSOLETE_WRITER_RECORD_CHILD";
    if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("1")) {
        let name = concat!(
            module_path!(),
            "::obsolete_migration_records_do_not_control_current_writer_custody"
        )
        .strip_prefix("hagency_platform::")
        .unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            result.status.success()
                && String::from_utf8_lossy(&result.stdout).contains("test result: ok. 1 passed;"),
            "{} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    for bytes in [
        br#"{"version":1,"registryId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","migrationId":"old","inventoryDigest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#.as_slice(),
        b"{malformed migration record".as_slice(),
        &[b'x'; 8192],
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("authority");
        private::directory(&root).unwrap();
        let marker = root.join("legacy-resource-refusal.json");
        let pending = root.join("legacy-resource-refusal.pending");
        private::write_new(&marker, bytes).unwrap();
        private::write_new(&pending, b"unfinished obsolete publication").unwrap();
        let shared = LegacyWriterGuard::isolated_for_tests(&root).unwrap();
        assert!(matches!(ExclusiveWriterGuard::isolated_for_tests(&root), Err(Error::Locked)));
        drop(shared);
        let exclusive = ExclusiveWriterGuard::isolated_for_tests(&root).unwrap();
        assert!(matches!(LegacyWriterGuard::isolated_for_tests(&root), Err(Error::Locked)));
        exclusive.check().unwrap();
        drop(exclusive);
        drop(LegacyWriterGuard::isolated_for_tests(&root).unwrap());
        assert_eq!(std::fs::read(marker).unwrap(), bytes);
        assert_eq!(std::fs::read(pending).unwrap(), b"unfinished obsolete publication");
    }
}
#[test]
fn retained_exclusive_custody_rejects_replaced_lock() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("authority");
    let guard = ExclusiveWriterGuard::isolated_for_tests(&root).unwrap();
    std::fs::rename(root.join(LOCK), root.join("old.lock")).unwrap();
    private::write_new(&root.join(LOCK), b"").unwrap();
    assert!(matches!(guard.check(), Err(Error::Private)));
}

#[test]
fn retained_exclusive_custody_rejects_replaced_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("authority");
    let guard = ExclusiveWriterGuard::isolated_for_tests(&root).unwrap();
    std::fs::rename(&root, temp.path().join("original")).unwrap();
    private::directory(&root).unwrap();
    private::write_new(&root.join(LOCK), b"").unwrap();
    assert!(matches!(guard.check(), Err(Error::Private)));
}
