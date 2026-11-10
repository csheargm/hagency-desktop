use super::*;

#[tokio::test]
async fn missing_absolute_provider_executable_never_creates_unknown_process_custody() {
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let paths = super::super::paths(&root.join("state"), "https://server.example/", "https://server.example/auth/", "subject", "@owner:test").unwrap();
    for executable in [root.join("missing-provider"),root.clone()] {
        let manager=OwnerProvider::default();
        let error=Process::spawn_inner(&paths,&executable,false,&manager.tasks).await.err().unwrap();
        assert_eq!(error.code,"codex_provider_unavailable");
        assert_eq!(manager.drain_for_migration().await,Ok(()));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn failed_provider_handshake_retains_and_reaps_the_actual_child() {
    use std::os::unix::fs::PermissionsExt;
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root=temp.path().canonicalize().unwrap();
    let paths=super::super::paths(&root.join("state"),"https://server.example/","https://server.example/auth/","subject","@owner:test").unwrap();
    let executable=root.join("malformed-provider.py");
    std::fs::write(&executable,"#!/usr/bin/env python3\nimport os,pathlib,time\npathlib.Path(os.environ['CODEX_HOME'],'fixture-child-pid').write_text(str(os.getpid()))\nprint('invalid-json',flush=True)\ntime.sleep(60)\n").unwrap();
    std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).unwrap();
    let manager=OwnerProvider::default();
    assert!(Process::spawn_inner(&paths,&executable,false,&manager.tasks).await.is_err());
    let pid=std::fs::read_to_string(paths.codex_home.join("fixture-child-pid")).unwrap();
    assert_eq!(manager.drain_for_migration().await,Ok(()));
    assert!(!std::process::Command::new("/bin/kill").args(["-0",pid.trim()]).stderr(std::process::Stdio::null()).status().unwrap().success());
}

#[tokio::test]
async fn dropped_real_child_is_reaped_before_drain_returns() {
    let tasks = TaskCustody::default();
    let activity = tasks.enter().unwrap();
    let child = tokio::process::Command::new("/bin/sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let id = child.id().unwrap();
    let child = TrackedChild::new(child, &tasks);
    let waiter = tokio::spawn({
        let tasks = tasks.clone();
        async move { tasks.drain().await }
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    drop(child);
    drop(activity);
    assert_eq!(waiter.await.unwrap(), Ok(()));
    // signal 0 does not act on the process; ESRCH is actual exit evidence.
    #[cfg(unix)]
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &id.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ordinary_provider_stop_retains_entry_during_cancelled_wait() {
    use std::os::unix::fs::PermissionsExt;
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let paths = super::super::paths(
        &root.join("state"),
        "https://server.example/",
        "https://server.example/auth/",
        "subject",
        "@owner:test",
    )
    .unwrap();
    let binary = root.join("provider-fixture");
    std::fs::write(&binary,"#!/usr/bin/env python3\nimport sys,json\nfor line in sys.stdin:\n r=json.loads(line)\n if 'id' in r: print(json.dumps({'id':r['id'],'result':{'userAgent':'fixture/0.167.0'}}),flush=True)\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let manager = OwnerProvider::default();
    let process = Process::spawn_inner(&paths, &binary, false, &manager.tasks)
        .await
        .unwrap();
    let entry = Arc::new(Entry {
        process: Mutex::new(process),
        paths,
    });
    manager
        .entries
        .lock()
        .await
        .insert("fixture".into(), entry.clone());
    let guard = entry.process.lock().await;
    let first = tokio::spawn({
        let manager = manager.clone();
        async move { manager.stop_all().await }
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    first.abort();
    let second = tokio::spawn({
        let manager = manager.clone();
        async move { manager.stop_all().await }
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!second.is_finished());
    drop(guard);
    second.await.unwrap();
    assert!(
        entry
            .process
            .lock()
            .await
            .child
            .try_wait()
            .unwrap()
            .is_some()
    );
    assert!(manager.entries.lock().await.is_empty());
    drop(entry);
    assert_eq!(manager.drain_for_migration().await, Ok(()));
}
