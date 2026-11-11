use super::*;
use crate::{Budget, Layer, Limit, Period, Policy, RequestPolicy};
use std::os::unix::fs::{PermissionsExt, symlink};
const OWNER: &str = "@alice:example.org";
#[test]
fn chat_directories_preserve_base_and_reject_linked_room_storage() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
    let first = chat_directory(&base, OWNER, &scope()).unwrap();
    assert_eq!(std::fs::metadata(&base).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(std::fs::metadata(&first).unwrap().permissions().mode() & 0o777, 0o700);
    let mut same_room = scope(); same_room.requester = "@another:example.org".into(); same_room.thread = "$other".into();
    assert_eq!(first, chat_directory(&base, OWNER, &same_room).unwrap());
    assert_ne!(first, chat_directory(&base, "@another:example.org", &scope()).unwrap());
    std::fs::remove_dir(&first).unwrap();
    symlink(&base, &first).unwrap();
    assert!(matches!(chat_directory(&base, OWNER, &scope()), Err(FileError::Boundary)));
}
fn scope() -> Scope {
    Scope {
        agent: "agent-a".into(),
        binding: "binding-a".into(),
        room: "!room-a:example.org".into(),
        requester: "@bob:example.org".into(),
        thread: "$thread".into(),
    }
}
fn setup() -> (tempfile::TempDir, Ledger, Workspace) {
    let temp = tempfile::tempdir().unwrap();
    let owner = temp.path().join(format!("owner_{}", "a".repeat(64)));
    std::fs::create_dir(&owner).unwrap();
    std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut ledger = Ledger::open(owner.join("agent-local.sqlite"), OWNER).unwrap();
    let s = scope();
    ledger.register_binding(OWNER, &s).unwrap();
    let owner = owner.canonicalize().unwrap();
    let workspace = Workspace::open(&owner, OWNER, &s).unwrap();
    let policy = policy(
        &workspace,
        ToolPolicy::AllowWithRules {
            tools: vec!["room.list".into(), "room.read".into(), "room.create".into()],
            directories: vec![workspace.canonical_directory().to_str().unwrap().into()],
        },
    );
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        ledger.set_policy(OWNER, &s, layer, 0, &policy).unwrap();
    }
    ledger
        .reserve(&s, "model-call", "dispatch-a", 10, 10)
        .unwrap();
    (temp, ledger, workspace)
}
fn policy(_workspace: &Workspace, high_risk: ToolPolicy) -> Policy {
    Policy {
        budget: Budget {
            limit: Limit::Tokens(1000),
            period: Period::Lifetime,
        },
        requests: RequestPolicy::Allow,
        high_risk,
    }
}
fn prepare(workspace: &Workspace, ledger: &Ledger, operation: Operation) -> PreparedCall {
    workspace
        .prepare(
            ledger,
            &scope(),
            "dispatch-a",
            "codex-thread-a",
            "turn-a",
            "tool-a",
            operation,
            11,
        )
        .unwrap()
}
fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
#[test]
fn real_create_read_list_replay_and_no_overwrite() {
    let (_t, mut ledger, mut workspace) = setup();
    let call = prepare(
        &workspace,
        &ledger,
        Operation::Create {
            path: "note.txt".into(),
            content: "room private".into(),
            nonce: "b".repeat(64),
        },
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &call, 12).unwrap(),
        FileResult::Created {
            replayed: false,
            ..
        }
    ));
    assert_eq!(
        std::fs::read_to_string(workspace.canonical_directory().join("note.txt")).unwrap(),
        "room private"
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &call, 12).unwrap(),
        FileResult::Created { replayed: true, .. }
    ));
    let read = prepare(
        &workspace,
        &ledger,
        Operation::Read {
            path: "note.txt".into(),
        },
    );
    assert!(
        matches!(workspace.execute(&mut ledger,&read,12).unwrap(),FileResult::Read {content,..} if content=="room private")
    );
    let list = prepare(
        &workspace,
        &ledger,
        Operation::List {
            path: String::new(),
        },
    );
    assert_eq!(
        workspace.execute(&mut ledger, &list, 12).unwrap(),
        FileResult::List {
            entries: vec!["note.txt".into()]
        }
    );
    let other = prepare(
        &workspace,
        &ledger,
        Operation::Create {
            path: "note.txt".into(),
            content: "overwrite".into(),
            nonce: "c".repeat(64),
        },
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &other, 12),
        Err(FileError::Conflict)
    ));
    assert_eq!(
        std::fs::read_to_string(workspace.canonical_directory().join("note.txt")).unwrap(),
        "room private"
    );
    write(
        &workspace.canonical_directory().join("note.txt"),
        b"externally changed",
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &call, 12),
        Err(FileError::Unknown)
    ));
}
#[test]
fn exact_owner_approval_is_single_use_and_policy_changes_deny() {
    let (_t, mut ledger, mut workspace) = setup();
    let snapshot = ledger.policy_snapshot(&scope()).unwrap();
    ledger
        .set_policy(
            OWNER,
            &scope(),
            Layer::Requester,
            snapshot[2].revision,
            &policy(&workspace, ToolPolicy::AskOwner),
        )
        .unwrap();
    let call = prepare(
        &workspace,
        &ledger,
        Operation::Create {
            path: "approved.txt".into(),
            content: "approved".into(),
            nonce: "d".repeat(64),
        },
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &call, 12),
        Err(FileError::NeedsOwner)
    ));
    assert!(
        !workspace
            .canonical_directory()
            .join("approved.txt")
            .exists()
    );
    ledger.approve_tool(OWNER, call.proposal(), 12).unwrap();
    assert!(workspace.execute(&mut ledger, &call, 12).is_ok());
    assert!(matches!(
        workspace.execute(&mut ledger, &call, 12),
        Ok(FileResult::Created { replayed: true, .. })
    ));
    assert!(ledger.authorize_tool(call.proposal(), 12).is_err());
    let next = prepare(
        &workspace,
        &ledger,
        Operation::Read {
            path: "approved.txt".into(),
        },
    );
    ledger.approve_tool(OWNER, next.proposal(), 12).unwrap();
    let revision = ledger.policy_snapshot(&scope()).unwrap()[2].revision;
    ledger
        .set_policy(
            OWNER,
            &scope(),
            Layer::Requester,
            revision,
            &policy(&workspace, ToolPolicy::Deny),
        )
        .unwrap();
    assert!(matches!(
        workspace.execute(&mut ledger, &next, 12),
        Err(FileError::Denied)
    ));
}
#[test]
fn path_symlink_hardlink_special_file_and_cross_binding_are_refused() {
    let (temp, mut ledger, mut workspace) = setup();
    for path in [
        "../secret",
        "/etc/passwd",
        "x/../y",
        "x//y",
        ".hagency-file-control/lock",
        "a\\b",
    ] {
        assert!(matches!(
            workspace.prepare(
                &ledger,
                &scope(),
                "dispatch-a",
                "codex-thread-a",
                "turn",
                "call",
                Operation::Read { path: path.into() },
                11
            ),
            Err(FileError::Invalid)
        ));
    }
    let victim = temp.path().join("victim");
    write(&victim, b"other room");
    symlink(&victim, workspace.canonical_directory().join("link")).unwrap();
    std::fs::hard_link(&victim, workspace.canonical_directory().join("hardlink")).unwrap();
    for path in ["link", "hardlink"] {
        let call = prepare(&workspace, &ledger, Operation::Read { path: path.into() });
        assert!(matches!(
            workspace.execute(&mut ledger, &call, 12),
            Err(FileError::Boundary)
        ));
    }
    symlink(temp.path(), workspace.canonical_directory().join("escape")).unwrap();
    let call = prepare(
        &workspace,
        &ledger,
        Operation::Create {
            path: "escape/should-not-exist".into(),
            content: "no".into(),
            nonce: "e".repeat(64),
        },
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &call, 12),
        Err(FileError::Boundary)
    ));
    assert!(!temp.path().join("should-not-exist").exists());
    let mut foreign = scope();
    foreign.binding = "binding-b".into();
    assert!(matches!(
        workspace.prepare(
            &ledger,
            &foreign,
            "dispatch-a",
            "codex-thread-a",
            "turn",
            "call",
            Operation::List {
                path: String::new()
            },
            11
        ),
        Err(FileError::Boundary)
    ));
    assert!(matches!(
        workspace.prepare(
            &ledger,
            &scope(),
            "dispatch-a",
            "codex-thread-a",
            "turn",
            "call",
            Operation::Replace {
                path: "file".into()
            },
            11
        ),
        Err(FileError::UnsupportedReplace)
    ));
}
#[test]
fn retained_root_bounds_reads_and_quarantines_unproved_publication() {
    let (_temp, mut ledger, mut workspace) = setup();
    write(
        &workspace.canonical_directory().join("large"),
        &vec![b'x'; BYTES + 1],
    );
    let read = prepare(
        &workspace,
        &ledger,
        Operation::Read {
            path: "large".into(),
        },
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &read, 12),
        Err(FileError::Capacity)
    ));
    let create = prepare(
        &workspace,
        &ledger,
        Operation::Create {
            path: "missing-proof".into(),
            content: "same content".into(),
            nonce: "f".repeat(64),
        },
    );
    write(
        &workspace.canonical_directory().join("missing-proof"),
        b"same content",
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &create, 12),
        Err(FileError::Conflict)
    ));
    assert!(matches!(
        workspace.execute(&mut ledger, &read, 400),
        Err(FileError::Denied)
    ));
    // A moved ambient pathname never redirects the already-held capability.
    let original = workspace.canonical_directory().to_path_buf();
    let moved = original.with_extension("moved");
    std::fs::rename(&original, &moved).unwrap();
    symlink("/", &original).unwrap();
    let read = prepare(
        &workspace,
        &ledger,
        Operation::Read {
            path: "etc/passwd".into(),
        },
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &read, 12),
        Err(FileError::Boundary)
    ));
}

#[test]
fn private_binding_lock_persistent_receipt_and_protocol_pins() {
    let (_temp, mut ledger, mut workspace) = setup();
    let owner = workspace
        .canonical_directory()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    assert!(matches!(
        Workspace::open(&owner, OWNER, &scope()),
        Err(FileError::Boundary)
    ));
    let mut other_scope = scope();
    other_scope.binding = "binding-b".into();
    other_scope.room = "!room-b:example.org".into();
    let other = Workspace::open(&owner, OWNER, &other_scope).unwrap();
    assert_ne!(other.canonical_directory(), workspace.canonical_directory());
    let call = prepare(
        &workspace,
        &ledger,
        Operation::Create {
            path: "durable.txt".into(),
            content: "durable".into(),
            nonce: "1".repeat(64),
        },
    );
    assert_eq!(call.proposal().arguments["codexThreadId"], "codex-thread-a");
    assert_eq!(call.proposal().arguments["turnId"], "turn-a");
    assert_eq!(call.proposal().arguments["callId"], "tool-a");
    workspace.execute(&mut ledger, &call, 12).unwrap();
    drop(workspace);
    let mut reopened = Workspace::open(&owner, OWNER, &scope()).unwrap();
    assert!(matches!(
        reopened.execute(&mut ledger, &call, 12),
        Ok(FileResult::Created { replayed: true, .. })
    ));
    let receipt = reopened
        .canonical_directory()
        .join(INTERNAL)
        .join(format!("receipt-{}", call.digest));
    write(&receipt, b"partial proof");
    assert!(matches!(
        reopened.execute(&mut ledger, &call, 12),
        Err(FileError::Unknown)
    ));
    assert_eq!(
        std::fs::read_to_string(reopened.canonical_directory().join("durable.txt")).unwrap(),
        "durable"
    );
}

#[test]
fn list_capacity_directory_permissions_and_fifo_are_closed() {
    let (_temp, mut ledger, mut workspace) = setup();
    let root = workspace.canonical_directory();
    std::fs::create_dir(root.join("directory")).unwrap();
    std::fs::set_permissions(
        root.join("directory"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let call = prepare(
        &workspace,
        &ledger,
        Operation::List {
            path: "directory".into(),
        },
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &call, 12),
        Err(FileError::Boundary)
    ));
    // Fixture setup only; production file tools never launch subprocesses.
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .args(["-m", "600"])
            .arg(workspace.canonical_directory().join("fifo"))
            .status()
            .unwrap()
            .success()
    );
    let read = prepare(
        &workspace,
        &ledger,
        Operation::Read {
            path: "fifo".into(),
        },
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &read, 12),
        Err(FileError::Boundary)
    ));
    std::fs::remove_file(workspace.canonical_directory().join("fifo")).unwrap();
    for i in 0..=LIST {
        write(
            &workspace.canonical_directory().join(format!("file-{i}")),
            b"x",
        );
    }
    let list = prepare(
        &workspace,
        &ledger,
        Operation::List {
            path: String::new(),
        },
    );
    assert!(matches!(
        workspace.execute(&mut ledger, &list, 12),
        Err(FileError::Capacity)
    ));
}

#[test]
fn shared_workspace_updates_existing_files_without_following_links() {
    let (temp, mut ledger, _) = setup();
    let owner = temp.path().join(format!("owner_{}", "a".repeat(64))).canonicalize().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(project.join("example.py"), "old\n").unwrap();
    let mut workspace = Workspace::open_shared(&owner, &project.canonicalize().unwrap(), OWNER, &scope()).unwrap();
    let allowed = policy(&workspace, ToolPolicy::AllowWithRules {
        tools: vec!["room.list".into(), "room.read".into(), "room.update".into()],
        directories: vec![workspace.canonical_directory().to_str().unwrap().into()],
    });
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        ledger.set_policy(OWNER, &scope(), layer, 1, &allowed).unwrap();
    }
    ledger.reserve(&scope(), "shared-call", "shared-dispatch", 10, 10).unwrap();
    let original = digest(b"old\n");
    let update = workspace.prepare(&ledger, &scope(), "shared-dispatch", "codex-thread-a", "turn-a", "tool-a", Operation::Update {
        path: "example.py".into(), content: "new\n".into(), expected_sha256: original, nonce: "d".repeat(64),
    }, 11).unwrap();
    let outcome = workspace.execute(&mut ledger, &update, 12);
    assert!(matches!(outcome, Ok(FileResult::Updated { .. })), "{outcome:?}");
    assert_eq!(std::fs::read_to_string(project.join("example.py")).unwrap(), "new\n");
    assert!(matches!(workspace.execute(&mut ledger, &update, 12), Err(FileError::Unknown)));
    let stale = workspace.prepare(&ledger, &scope(), "shared-dispatch", "codex-thread-a", "turn-a", "tool-c", Operation::Update {
        path: "example.py".into(), content: "lost update\n".into(), expected_sha256: digest(b"old\n"), nonce: "e".repeat(64),
    }, 11).unwrap();
    assert!(matches!(workspace.execute(&mut ledger, &stale, 12), Err(FileError::Conflict)));
    assert_eq!(std::fs::read_to_string(project.join("example.py")).unwrap(), "new\n");
    let outside = temp.path().join("outside");
    std::fs::write(&outside, "safe").unwrap();
    symlink(&outside, project.join("link")).unwrap();
    let listing = workspace.prepare(&ledger, &scope(), "shared-dispatch", "codex-thread-a", "turn-a", "tool-list", Operation::List { path: "".into() }, 11).unwrap();
    assert!(matches!(workspace.execute(&mut ledger, &listing, 12), Ok(FileResult::List { entries }) if entries == vec!["example.py"]));
    let read = workspace.prepare(&ledger, &scope(), "shared-dispatch", "codex-thread-a", "turn-a", "tool-b", Operation::Read { path: "link".into() }, 11).unwrap();
    assert!(matches!(workspace.execute(&mut ledger, &read, 12), Err(FileError::Boundary)));
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "safe");
    assert!(workspace.prepare(&ledger, &scope(), "shared-dispatch", "codex-thread-a", "turn-a", "tool-d", Operation::Read { path: "../outside".into() }, 11).is_err());
    let mut foreign = scope(); foreign.binding = "another-binding".into();
    assert!(matches!(workspace.prepare(&ledger, &foreign, "shared-dispatch", "codex-thread-a", "turn-a", "tool-e", Operation::Read { path: "example.py".into() }, 11), Err(FileError::Boundary)));
    let ask = policy(&workspace, ToolPolicy::AskOwner);
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        ledger.set_policy(OWNER, &scope(), layer, 2, &ask).unwrap();
    }
    ledger.reserve(&scope(), "approval-call", "approval-dispatch", 10, 10).unwrap();
    let change = workspace.prepare(&ledger, &scope(), "approval-dispatch", "codex-thread-a", "turn-a", "tool-f", Operation::Update {
        path: "example.py".into(), content: "approved\n".into(), expected_sha256: digest(b"new\n"), nonce: "f".repeat(64),
    }, 11).unwrap();
    assert!(matches!(workspace.execute(&mut ledger, &change, 12), Err(FileError::NeedsOwner)));
    assert_eq!(std::fs::read_to_string(project.join("example.py")).unwrap(), "new\n");
    ledger.approve_tool(OWNER, change.proposal(), 12).unwrap();
    assert!(matches!(workspace.execute(&mut ledger, &change, 12), Ok(FileResult::Updated { .. })));
    assert_eq!(std::fs::read_to_string(project.join("example.py")).unwrap(), "approved\n");
    std::fs::rename(&project, temp.path().join("moved-project")).unwrap();
    std::fs::create_dir(&project).unwrap();
    let changed = workspace.prepare(&ledger, &scope(), "approval-dispatch", "codex-thread-a", "turn-a", "tool-g", Operation::Read { path: "example.py".into() }, 11).unwrap();
    assert!(matches!(workspace.execute(&mut ledger, &changed, 12), Err(FileError::Boundary)));
}

#[test]
fn shared_workspace_supports_three_agent_handoff() {
    let (temp, mut ledger, _) = setup();
    let owner = temp.path().join(format!("owner_{}", "a".repeat(64))).canonicalize().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let project = project.canonicalize().unwrap();
    let mut scopes = Vec::new();
    let mut workspaces = Vec::new();
    for agent in ["architect", "tester", "coder"] {
        let mut current = scope();
        current.agent = agent.into(); current.binding = format!("binding-{agent}");
        ledger.register_binding(OWNER, &current).unwrap();
        let workspace = Workspace::open_shared(&owner, &project, OWNER, &current).unwrap();
        let allowed = policy(&workspace, ToolPolicy::AllowWithRules {
            tools: vec!["room.list".into(), "room.read".into(), "room.create".into(), "room.mkdir".into(), "room.update".into()],
            directories: vec![project.to_str().unwrap().into()],
        });
        for layer in [Layer::Agent, Layer::Room, Layer::Requester] { ledger.set_policy(OWNER, &current, layer, 0, &allowed).unwrap(); }
        ledger.reserve(&current, &format!("call-{agent}"), &format!("dispatch-{agent}"), 10, 10).unwrap();
        scopes.push(current); workspaces.push(workspace);
    }
    let prepare = |workspace: &Workspace, ledger: &Ledger, i: usize, id: &str, operation| {
        workspace.prepare(ledger, &scopes[i], &format!("dispatch-{}", ["architect", "tester", "coder"][i]), "thread", "turn", id, operation, 11).unwrap()
    };
    let lock = workspaces[0].shared_lock.as_ref().unwrap();
    lock.try_lock().unwrap();
    assert!(workspaces[1].shared_lock.as_ref().unwrap().try_lock().is_err());
    lock.unlock().unwrap();
    let mkdir = prepare(&workspaces[0], &ledger, 0, "mkdir", Operation::MakeDirectory { path: "role-e2e".into() });
    assert!(matches!(workspaces[0].execute(&mut ledger, &mkdir, 12), Ok(FileResult::DirectoryCreated { .. })));
    let adr = prepare(&workspaces[0], &ledger, 0, "adr", Operation::Create { path: "role-e2e/ADR.md".into(), content: "Implement count\n".into(), nonce: "a".repeat(64) });
    assert!(matches!(workspaces[0].execute(&mut ledger, &adr, 12), Ok(FileResult::Created { .. })));
    let read = prepare(&workspaces[1], &ledger, 1, "read", Operation::Read { path: "role-e2e/ADR.md".into() });
    assert!(matches!(workspaces[1].execute(&mut ledger, &read, 12), Ok(FileResult::Read { content, .. }) if content == "Implement count\n"));
    let test = prepare(&workspaces[1], &ledger, 1, "test", Operation::Create { path: "role-e2e/test_count.py".into(), content: "assert count('a') == 1\n".into(), nonce: "b".repeat(64) });
    assert!(matches!(workspaces[1].execute(&mut ledger, &test, 12), Ok(FileResult::Created { .. })));
    std::fs::write(project.join("role-e2e/count.py"), "old\n").unwrap();
    let update = prepare(&workspaces[2], &ledger, 2, "implementation", Operation::Update { path: "role-e2e/count.py".into(), content: "def count(text): return len(text.split())\n".into(), expected_sha256: digest(b"old\n"), nonce: "c".repeat(64) });
    assert!(matches!(workspaces[2].execute(&mut ledger, &update, 12), Ok(FileResult::Updated { .. })));
    assert_eq!(std::fs::read_to_string(project.join("role-e2e/count.py")).unwrap(), "def count(text): return len(text.split())\n");
}
