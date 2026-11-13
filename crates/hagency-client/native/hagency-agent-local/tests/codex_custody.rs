#![cfg(unix)]
use hagency_agent_local::{codex::Profile, runtime};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

async fn wait_for(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("offline process reached checkpoint");
}

async fn wait_for_heartbeat_change(path: &Path, previous: &[u8]) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let current = fs::read(path).unwrap();
            if !current.is_empty() && current.as_slice() != previous {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("descendant did not advance its heartbeat before cleanup");
}

#[tokio::test]
async fn codex_stop_and_drop_reap_native_children_after_leader_exit() {
    for (exit_leader, drop_owner) in [(true, false), (true, true), (false, false), (false, true)] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for name in ["home", "provider", "work"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        let actor = root.join("actor");
        let script = r#"#!/usr/bin/env python3
import json, pathlib, subprocess, sys, time
root = pathlib.Path(__ROOT__)
exit_leader = __EXIT__
request = json.loads(sys.stdin.readline())
child = subprocess.Popen([sys.executable, '-c', '''
import pathlib, sys, time
root = pathlib.Path(sys.argv[1])
until = time.monotonic() + 20
while time.monotonic() < until:
    (root / 'heartbeat').write_text(str(time.monotonic_ns()))
    time.sleep(0.01)
''', str(root)], start_new_session=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
while not (root / 'heartbeat').exists(): time.sleep(0.005)
sys.stderr.write('x' * 262144)
sys.stderr.flush()
print(json.dumps({'id':request['id'], 'result':{'userAgent':'offline/0.167.0'}}), flush=True)
json.loads(sys.stdin.readline())
if exit_leader:
    (root / 'leader-exited').write_text('done')
else:
    for line in sys.stdin: pass
"#.replace("__ROOT__", &serde_json::to_string(&root).unwrap())
   .replace("__EXIT__", if exit_leader {"True"} else {"False"});
        fs::write(&actor, script).unwrap();
        fs::set_permissions(&actor, fs::Permissions::from_mode(0o700)).unwrap();
        let profile = Profile {
            executable: actor,
            home: root.join("home"),
            codex_home: root.join("provider"),
            cwd: root.join("work"),
            model: "offline".into(),
            effort: "medium".into(),
            shared_auth: true,
            web_search: false,
        };
        let guardian = Path::new(env!("CARGO_BIN_EXE_hagency-local-probe"));
        let mut process = runtime::spawn_with_guardian(&profile, "codex", Some(guardian))
            .await
            .unwrap();
        process.session.initialize().await.unwrap();
        if exit_leader {
            wait_for(&root.join("leader-exited")).await;
        }
        let heartbeat = root.join("heartbeat");
        if !exit_leader {
            let before = fs::read(&heartbeat).unwrap();
            // Scheduling a timer is not evidence that the child was scheduled.
            wait_for_heartbeat_change(&heartbeat, &before).await;
        }
        if !drop_owner {
            process.stop().await.expect("observed whole-tree cleanup");
            process.stop().await.expect("cleanup is idempotent");
        }
        drop(process);
        let stopped = fs::read(&heartbeat).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            stopped,
            fs::read(&heartbeat).unwrap(),
            "descendant outlived its owner"
        );
    }
}
