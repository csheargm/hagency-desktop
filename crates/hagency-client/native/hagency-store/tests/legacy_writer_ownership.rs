//! Actual normal openers under one explicitly selected synthetic installation.
use hagency_platform::legacy_writer::{self, ExclusiveWriterGuard};
use hagency_store::{DomainRepository, Error};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("authority");
    hagency_store::private::directory(&root).unwrap();
    let source = temp.path().join("source");
    (temp, root, source)
}
fn command(root: &Path, source: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["legacy_writer_child", "--exact", "--nocapture"])
        .env("HAGENCY_NORMAL_WRITER_ROOT", root)
        .env("HAGENCY_NORMAL_WRITER_SOURCE", source)
        .env("HAGENCY_NORMAL_WRITER_MODE", mode);
    command
}
struct WriterChild(Child);
impl WriterChild {
    fn spawn(root: &Path, source: &Path) -> Self {
        Self(
            command(root, source, "hold_both")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    }
    fn send(&mut self, byte: u8) {
        self.0.stdin.as_mut().unwrap().write_all(&[byte]).unwrap();
    }
}
impl Drop for WriterChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn ready(path: &Path, child: &mut WriterChild) {
    let until = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < until, "writer fixture timed out");
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "writer exited before acknowledgement"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn run(root: &Path, source: &Path, mode: &str) {
    let output = command(root, source, mode).output().unwrap();
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn legacy_writer_child() {
    let Some(root) = std::env::var_os("HAGENCY_NORMAL_WRITER_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let source = PathBuf::from(std::env::var_os("HAGENCY_NORMAL_WRITER_SOURCE").unwrap());
    let mode = std::env::var("HAGENCY_NORMAL_WRITER_MODE").unwrap();
    if mode == "missing_root" {
        assert!(matches!(
            legacy_writer::LegacyWriterGuard::acquire(),
            Err(legacy_writer::Error::TestRoot)
        ));
        assert!(legacy_writer::test_support::initialize(&root).is_err());
        assert!(!source.exists());
        return;
    }
    legacy_writer::test_support::initialize(&root).unwrap();
    match mode.as_str() {
        "fixed_root" => {
            let _guard = legacy_writer::LegacyWriterGuard::acquire().unwrap();
            let alternate = root.join("alternate");
            hagency_store::private::directory(&alternate).unwrap();
            assert!(legacy_writer::test_support::initialize(&alternate).is_err());
            legacy_writer::test_support::initialize(&root).unwrap();
        }
        "hold_both" => {
            let domain = DomainRepository::open(&source).unwrap();
            let personal = hagency_agent_local::Ledger::open(
                source.join("personal.sqlite"),
                "@fixture:example.test",
            )
            .unwrap();
            std::fs::write(root.join("ready"), b"ready").unwrap();
            let mut byte = [0];
            std::io::stdin().read_exact(&mut byte).unwrap();
            assert_eq!(byte[0], b'd');
            drop(domain);
            std::fs::write(root.join("domain-dropped"), b"dropped").unwrap();
            std::io::stdin().read_exact(&mut byte).unwrap();
            assert_eq!(byte[0], b'p');
            drop(personal);
        }
        "current_open" => {
            let domain = DomainRepository::open(&source).unwrap();
            let personal = hagency_agent_local::Ledger::open(
                source.join("personal.sqlite"),
                "@fixture:example.test",
            )
            .unwrap();
            assert!(matches!(
                ExclusiveWriterGuard::isolated_for_tests(&root),
                Err(legacy_writer::Error::Locked)
            ));
            drop(domain);
            drop(personal);
        }
        "locked" => {
            for result in [
                DomainRepository::open(&source),
                DomainRepository::open_managed(&source, &"a".repeat(64)),
            ] {
                assert!(matches!(result, Err(Error::Locked)));
            }
            assert!(matches!(
                hagency_agent_local::Ledger::open(
                    source.join("personal.sqlite"),
                    "@fixture:example.test"
                ),
                Err(hagency_agent_local::Error::Installation(
                    legacy_writer::Error::Locked
                ))
            ));
            let out = root.join("forbidden-snapshot");
            for result in [
                hagency_store::backup::snapshot(&source, &out, 1),
                hagency_store::backup::restore(&out, &source),
            ] {
                assert!(matches!(result, Err(Error::Locked)));
            }
            assert!(!out.exists());
        }
        #[cfg(feature = "test-support")]
        "canonical" => {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let registry =
                    hagency_store::owner_resources::OwnerResourceRegistry::isolated_for_tests(
                        &root,
                    )
                    .unwrap();
                assert_eq!(registry.id().len(), 64);
                registry.shutdown().await.unwrap();
            });
        }
        _ => panic!("unexpected fixture mode"),
    }
}

#[test]
fn test_authority_root_must_be_explicit_and_fixed_before_first_open() {
    let (_temp, root, source) = fixture();
    run(&root, &source, "missing_root");
    run(&root, &source, "fixed_root");
}

#[test]
fn real_domain_and_personal_connections_share_installation_exclusion_until_both_drop() {
    let (_temp, root, source) = fixture();
    let mut child = WriterChild::spawn(&root, &source);
    ready(&root.join("ready"), &mut child);
    assert!(matches!(
        ExclusiveWriterGuard::isolated_for_tests(&root),
        Err(legacy_writer::Error::Locked)
    ));
    child.send(b'd');
    ready(&root.join("domain-dropped"), &mut child);
    assert!(matches!(
        ExclusiveWriterGuard::isolated_for_tests(&root),
        Err(legacy_writer::Error::Locked)
    ));
    child.send(b'p');
    assert!(child.0.wait().unwrap().success());
    ExclusiveWriterGuard::isolated_for_tests(&root)
        .unwrap()
        .check()
        .unwrap();
}
#[test]
fn all_normal_open_modes_refuse_before_creating_source_under_exclusive_custody() {
    let (_temp, root, source) = fixture();
    let _exclusive = ExclusiveWriterGuard::isolated_for_tests(&root).unwrap();
    run(&root, &source, "locked");
    assert!(!source.exists());
}
#[test]
fn obsolete_migration_records_cannot_block_current_writers_or_process_restart() {
    for marker in [
        br#"{"version":1,"registryId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","migrationId":"original","inventoryDigest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#.as_slice(),
        b"obsolete malformed record".as_slice(),
    ] {
        let (_temp, root, source) = fixture();
        let path = root.join("legacy-resource-refusal.json");
        hagency_store::private::write_new(&path, marker).unwrap();
        run(&root, &source, "current_open");
        let guard = ExclusiveWriterGuard::isolated_for_tests(&root).unwrap();
        run(&root, &source, "locked");
        guard.check().unwrap();
        drop(guard);
        run(&root, &source, "current_open");
        assert!(source.join("domain.sqlite3").is_file());
        assert!(source.join("personal.sqlite").is_file());
        assert_eq!(std::fs::read(path).unwrap(), marker);
    }
}
#[test]
#[cfg(feature = "test-support")]
fn sealed_canonical_registry_opener_coexists_with_exclusive_installation_lock() {
    let (_temp, root, source) = fixture();
    let _exclusive = ExclusiveWriterGuard::isolated_for_tests(&root).unwrap();
    run(&root, &source, "canonical");
    assert!(root.join("domain.sqlite3").exists());
    assert!(!source.exists());
}
