//! Codex push freely (ADR 0022, "Codex"): the host tool takes exactly the
//! Claude `Bash` steps, its file changes only inside the own checkout.
use super::super::fix_flow_tests::{OWNER, REPO, ROOM, Run, git, proposal, run_under};
use super::*;
use crate::{ToolPolicy, ToolProposal};

fn step(root: &Path, line: &str) -> ToolProposal {
    proposal(root, ROOM, TOOL, json!({"command": line}))
}
fn change(root: &Path, changes: Value) -> ToolProposal {
    let item = json!({"id": "file-a", "type": "fileChange", "status": "inProgress", "changes": changes});
    proposal(root, ROOM, "codex.file_change", json!({"request": {"threadId": "t", "turnId": "u", "itemId": "file-a"}, "item": item}))
}
fn update(path: &Path) -> Value {
    json!({"path": path, "kind": {"type": "update", "move_path": null}, "diff": "-teh\n+the"})
}

/// A Deny ledger (a hosted Agent's default) with the watched repository at `app`.
fn cloned() -> Run {
    let r = run_under(ToolPolicy::Deny);
    git(&r.root, &["clone", "-q", r.target.to_str().unwrap(), "app"]);
    git(&r.root.join("app"), &["remote", "set-url", "origin", &format!("https://github.com/{REPO}.git")]);
    r
}

#[test]
fn the_tool_takes_one_command_line_only() {
    assert_eq!(command_of(&json!({"command": "git status"})).as_deref(), Some("git status"));
    for bad in [json!({}), json!({"command": ""}), json!({"command": 7}), json!({"command": "git status", "cwd": "/"}), json!({"command": "a\0b"}), json!("git status")] {
        assert_eq!(command_of(&bad), None, "{bad}");
    }
    assert_eq!(command_of(&json!({"command": "x".repeat(MAX_COMMAND_BYTES + 1)})), None);
    assert_eq!(definition()["name"], MCP_TOOL);
    // The Agent's own watch block tells a Codex Agent to use it.
    assert!(crate::watch_info::PUSH_FREELY_STEPS.contains(MCP_TOOL));
    // A Codex Agent that watches nothing learns its room reads go here (e2e 8),
    // from its developer instructions and from the tool itself.
    assert!(hagency_runtime::CODEX_ROOM_REPO_READS.contains(MCP_TOOL));
    assert!(hagency_runtime::AGENT_MERGE_NOTE.contains(hagency_runtime::CODEX_ROOM_REPO_READS));
    assert!(definition()["description"].as_str().unwrap().contains(hagency_runtime::CODEX_ROOM_REPO_READS));
    // e2e 8 rerun: a native `gh api user` to learn its login cost a 5-minute
    // card, after which the room's set (5-minute record) had expired.
    assert!(hagency_runtime::REVIEW_STEPS.contains("use --comment too when you cannot tell which account you post as, never running a command only to find out"));
}

#[test]
fn host_tool_steps_follow_the_bash_allowlist() {
    let mut r = cloned();
    for allowed in ["git -C app status", "git -C app add README.md", "git -C app commit -m 'fix'", "git -C app push -u origin main", &format!("gh issue view 1 -R {REPO}")] {
        let call = step(&r.root, allowed);
        // `push origin main` is the default branch: refused by the push rule.
        let expected = !allowed.contains("push");
        assert_eq!(r.ledger.trusted_publish(OWNER, &call, 11).unwrap().is_some(), expected, "{allowed}");
    }
    for refused in [
        "git -C app status && git -C app push origin x",
        "git -C app status | sh",
        "git -C app config core.hooksPath /tmp",
        "git -C app -c core.hooksPath=/tmp status",
        "git -C ../outside status",
        "git -C app commit -m x -F /etc/passwd",
        "GIT_DIR=/tmp git -C app status",
        "sh -c 'git -C app status'",
        "curl https://example.com",
        "cargo test",
    ] {
        assert_eq!(r.ledger.trusted_publish(OWNER, &step(&r.root, refused), 11).unwrap(), None, "{refused}");
    }
}

#[test]
fn codex_native_shell_is_never_trusted() {
    let mut r = cloned();
    let native = proposal(&r.root, ROOM, "codex.command", json!({"command": "git -C app status", "cwd": r.root}));
    assert_eq!(r.ledger.trusted_publish(OWNER, &native, 11).unwrap(), None);
}

#[test]
fn a_file_change_inside_the_checkout_is_card_free() {
    let mut r = cloned();
    let app = r.root.join("app");
    let added = json!({"path": app.join("docs/new.md"), "kind": {"type": "add"}, "diff": "+x"});
    let renamed = json!({"path": app.join("README.md"), "kind": {"type": "update", "move_path": app.join("README.txt")}, "diff": ""});
    for changes in [json!([update(&app.join("README.md"))]), json!([added]), json!([renamed]), json!([{"path": app.join("README.md"), "kind": "update", "diff": ""}])] {
        assert_eq!(r.ledger.trusted_publish(OWNER, &change(&r.root, changes.clone()), 11).unwrap(), Some(TrustedStep::Allow), "{changes}");
    }
}

#[test]
fn file_change_escapes_ask() {
    let mut r = cloned();
    let app = r.root.join("app");
    std::os::unix::fs::symlink("/etc", app.join("etc-link")).unwrap();
    std::os::unix::fs::symlink(app.join(".git"), app.join("meta")).unwrap();
    let inside = update(&app.join("README.md"));
    let escapes = [
        json!([inside, update(&r.root.join("notes.md"))]),
        json!([update(Path::new("/etc/hosts"))]),
        json!([update(&app.join("../notes.md"))]),
        json!([update(&app.join("etc-link/hosts"))]),
        json!([update(&app.join(".git/hooks/pre-commit"))]),
        json!([update(&app.join("meta/config"))]),
        json!([update(&app.join(".gitattributes"))]),
        json!([{"path": app.join("README.md"), "kind": {"type": "update", "move_path": "/tmp/README.md"}, "diff": ""}]),
        json!([{"path": app.join("README.md"), "kind": {"type": "update", "movePath": app.join(".git/hooks/post-commit")}, "diff": ""}]),
        json!([{"path": app.join("README.md"), "kind": {"type": "delete"}, "diff": ""}]),
        json!([{"path": "README.md", "kind": {"type": "update"}, "diff": ""}]),
        json!([{"path": app.join("README.md"), "kind": {"type": "chmod"}, "diff": ""}]),
        json!([]),
    ];
    for changes in escapes {
        assert_eq!(r.ledger.trusted_publish(OWNER, &change(&r.root, changes.clone()), 11).unwrap(), None, "{changes}");
    }
    let mut granted = change(&r.root, json!([update(&app.join("README.md"))]));
    granted.arguments["request"]["grantRoot"] = json!("/");
    assert_eq!(r.ledger.trusted_publish(OWNER, &granted, 11).unwrap(), None, "a grant root asks");
    let mut stranger = change(&r.root, json!([update(&app.join("README.md"))]));
    stranger.scope.requester = "@mallory:example.org".into();
    assert_eq!(r.ledger.trusted_publish(OWNER, &stranger, 11).unwrap(), None, "another requester asks");
    let mut expansion = change(&r.root, json!([update(&app.join("README.md"))]));
    expansion.risk = "permission_expansion".into();
    assert_eq!(r.ledger.trusted_publish(OWNER, &expansion, 11).unwrap(), None);
}

#[test]
fn a_step_runs_pinned_in_the_workspace_and_a_file_edit_never_runs() {
    let r = cloned();
    let ctx = HostCtx::new(None, std::sync::Arc::default(), Box::new(|| true));
    let pinned = super::super::local_git::pinned_git(&r.root.join("app"), &["status", "--short", "--branch"]);
    assert!(run(TrustedStep::Command(pinned), &r.root, &ctx).is_ok_and(|out| out.starts_with("## main")));
    assert!(run(TrustedStep::Allow, &r.root, &ctx).is_err());
    // The model's environment never reaches the step: only the host's few
    // variables, and an empty gh config.
    let env = run(TrustedStep::Command("env".into()), &r.root, &ctx).unwrap();
    assert!(env.lines().all(|l| ["PATH=", "HOME=", "USER=", "LOGNAME=", "TMPDIR=", "LANG=", "GH_CONFIG_DIR=", "GIT_CONFIG_NOSYSTEM=", "GIT_CONFIG_GLOBAL=", "GIT_TERMINAL_PROMPT=", "GH_PROMPT_DISABLED=", "PWD=", "SHLVL=", "_="].iter().any(|k| l.starts_with(k))), "{env}");
    let token = HostCtx::new(Some("agent-token".into()), std::sync::Arc::default(), Box::new(|| true));
    assert!(run(TrustedStep::Command("printf %s \"$GH_TOKEN\"".into()), &r.root, &token).is_ok_and(|t| t == "agent-token"));
}

/// Regression (Nov 17): a review post with no Agent token reached GitHub as
/// the owner: the step kept the owner's `HOME`, so `gh` read the owner's
/// keychain login. A tokenless `gh` step is refused before it runs, and even
/// a `gh` the guard does not see finds no login in the step's environment
/// (`gh auth token` is local, no network call).
#[test]
fn a_step_without_a_token_never_signs_in_as_the_owner() {
    let r = cloned();
    let ctx = HostCtx::new(None, std::sync::Arc::default(), Box::new(|| true));
    let home = run(TrustedStep::Command("printf %s \"$HOME\"".into()), &r.root, &ctx).unwrap();
    assert!(std::env::var_os("HOME").is_none_or(|h| h.to_string_lossy() != home), "the step kept the host's HOME");
    let review = "GH_HOST='github.com' gh pr review 70 --approve -R csheargm/hagency-demo-sandbox -b 'Hagency verdict: APPROVE'";
    assert_eq!(run(TrustedStep::Command(review.into()), &r.root, &ctx), Err(super::super::step_env::NO_TOKEN.into()), "gh never ran");
    let blank = HostCtx::new(Some("  ".into()), std::sync::Arc::default(), Box::new(|| true));
    assert_eq!(run(TrustedStep::Command(review.into()), &r.root, &blank), Err(super::super::step_env::NO_TOKEN.into()));
    if std::process::Command::new("gh").arg("--version").output().is_ok_and(|o| o.status.success()) {
        let login = run(TrustedStep::Command("env gh auth token --hostname github.com".into()), &r.root, &ctx);
        assert!(login.is_err(), "gh found a stored login (the owner's)");
    }
}

/// User flow: bird-codex, hosted under Deny, is handed issue #1 and fixes
/// it end to end through the host tool and its own file change, zero cards.
#[test]
fn a_codex_hand_off_fixes_an_issue_with_zero_cards() {
    let mut r = run_under(ToolPolicy::Deny);
    let (root, target) = (r.root.clone(), r.target.clone());
    let app = root.join("hagency-demo-sandbox");
    let ctx = HostCtx::new(None, std::sync::Arc::default(), Box::new(|| true));
    let dest = super::super::host_git::Dest { url: format!("file://{}", target.display()), protocol: "file" };
    let mut cards = 0;
    let mut call = |ledger: &mut crate::Ledger, line: &str| -> String {
        let Some(step) = ledger.trusted_publish(OWNER, &step(&root, line), 11).unwrap() else {
            cards += 1;
            return String::new();
        };
        // `gh` would reach the real GitHub: answered without a card is enough here.
        if line.starts_with("gh pr") {
            return String::new();
        }
        let out = run_with(step, &root, &ctx, |host, ctx| host.run_to(&dest, ctx));
        out.unwrap_or_else(|e| panic!("{line}: {e}"))
    };
    call(&mut r.ledger, "gh repo clone csheargm/hagency-demo-sandbox ./hagency-demo-sandbox");
    call(&mut r.ledger, "git -C hagency-demo-sandbox fetch origin");
    call(&mut r.ledger, "git -C hagency-demo-sandbox checkout -b fix/typo origin/main");
    // Codex's own apply_patch, answered natively.
    let readme = app.join("README.md");
    let edit = change(&root, json!([update(&readme)]));
    assert_eq!(r.ledger.trusted_publish(OWNER, &edit, 11).unwrap(), Some(TrustedStep::Allow));
    std::fs::write(&readme, "# Sandbox\nTypo: the\n").unwrap();
    assert!(call(&mut r.ledger, "git -C hagency-demo-sandbox status --short").contains("README.md"));
    call(&mut r.ledger, "git -C hagency-demo-sandbox add README.md");
    // The committer identity, as the owner's git config would give it.
    git(&app, &["config", "user.name", "bird"]);
    git(&app, &["config", "user.email", "b@x"]);
    call(&mut r.ledger, "git -C hagency-demo-sandbox commit -m 'docs: fix typo (#1)'");
    let pushed = call(&mut r.ledger, "git -C hagency-demo-sandbox push -u origin fix/typo");
    assert!(pushed.starts_with("Pushed fix/typo"), "{pushed}");
    call(&mut r.ledger, "gh pr create --draft --repo csheargm/hagency-demo-sandbox --head fix/typo --title 'docs: fix typo' --body 'Fixes #1'");
    assert_eq!(cards, 0, "the whole fix ran without a card");
    assert_eq!(git(&target, &["log", "-1", "--format=%s", "refs/heads/fix/typo"]), "docs: fix typo (#1)");
}

/// GPT review: a PR body file is published under the Agent's token, so it
/// must be a regular file of the watched repository's own checkout.
#[test]
fn a_pr_body_file_comes_only_from_the_own_checkout() {
    let mut r = cloned();
    let app = r.root.join("app");
    std::fs::write(app.join("body.md"), "Fixes #1").unwrap();
    std::fs::write(r.root.join("secret.txt"), "token").unwrap();
    std::os::unix::fs::symlink(r.root.join("secret.txt"), app.join("link.md")).unwrap();
    let pr = |file: &Path| format!("gh pr create --repo {REPO} --head fix --title t --body-file {}", file.display());
    assert!(r.ledger.trusted_publish(OWNER, &step(&r.root, &pr(&app.join("body.md"))), 11).unwrap().is_some());
    for refused in [r.root.join("secret.txt"), app.join(".git/config"), app.join("link.md"), app.join("missing.md"), app.clone()] {
        assert_eq!(r.ledger.trusted_publish(OWNER, &step(&r.root, &pr(&refused)), 11).unwrap(), None, "{}", refused.display());
    }
}
