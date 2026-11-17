//! The turn side of `watched_repo_step`: a call outside push freely is
//! refused at once and runs nothing; one inside runs on the host and its
//! output reaches the model; a room without native commands refuses.
use super::*;
use crate::watch_trust::fix_flow_tests::{OWNER, REPO, ROOM, git, proposal, run_under};
use crate::ToolPolicy;

async fn ask(ledger: &mut Ledger, turn: &Turn<'_>, command: &str, running: &mut Running) -> (String, bool) {
    let (reply, answer) = oneshot::channel();
    start(ledger, turn, Call { command: command.into(), reply }, running);
    answer.await.unwrap()
}

#[tokio::test]
async fn a_turn_runs_only_card_free_steps() {
    let mut r = run_under(ToolPolicy::Deny);
    git(&r.root, &["clone", "-q", r.target.to_str().unwrap(), "app"]);
    git(&r.root.join("app"), &["remote", "set-url", "origin", &format!("https://github.com/{REPO}.git")]);
    let scope = proposal(&r.root, ROOM, "codex.repo_step", serde_json::json!({})).scope;
    let workspace = r.root.to_string_lossy().into_owned();
    let mut turn = Turn { owner: OWNER, scope: &scope, dispatch: "d", workspace: &workspace, token: None, enabled: true };
    let mut running = Running::default();
    let (out, failed) = ask(&mut r.ledger, &turn, "git -C app status --short --branch", &mut running).await;
    assert!(!failed && out.starts_with("## main"), "{out}");
    let marker = r.root.join("ran");
    let (out, failed) = ask(&mut r.ledger, &turn, &format!("git -C app status; touch {}", marker.display()), &mut running).await;
    assert!(failed && out.contains("not_card_free"), "{out}");
    assert!(!marker.exists(), "a refused call runs nothing");
    // A call given up while queued (its MCP wait expired) never starts.
    let (reply, answer) = oneshot::channel();
    drop(answer);
    start(&mut r.ledger, &turn, Call { command: "git -C app checkout -b late origin/main".into(), reply }, &mut running);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(std::process::Command::new("git").args(["-C", r.root.join("app").to_str().unwrap(), "rev-parse", "--verify", "-q", "late"])
        .status().is_ok_and(|s| !s.success()), "the abandoned step ran");
    turn.enabled = false;
    let (out, failed) = ask(&mut r.ledger, &turn, "git -C app status", &mut running).await;
    assert!(failed && out.contains("runs no commands"), "{out}");
}

#[tokio::test]
async fn forward_hands_the_call_to_its_turn_and_stops_with_it() {
    let (_turn, cancel) = tokio::sync::watch::channel(false);
    let wait = std::time::Duration::from_secs(5);
    let args = serde_json::json!({"command": "git status"});
    assert_eq!(forward(None, &args, cancel.clone(), wait).await, Err("repo_steps_unavailable"));
    let (sender, mut calls) = channel();
    assert_eq!(forward(Some(sender.clone()), &serde_json::json!({"cmd": 1}), cancel.clone(), wait).await, Err("arguments_invalid"));
    let answered = tokio::spawn(async move {
        let call = calls.recv().await.unwrap();
        assert_eq!(call.command, "git status");
        let _ = call.reply.send(("clean".into(), false));
        calls
    });
    assert_eq!(forward(Some(sender.clone()), &args, cancel.clone(), wait).await, Ok(("clean".into(), false)));
    let mut calls = answered.await.unwrap();
    // The turn ends while the step waits: the call is refused, not left hanging.
    let (end, cancel) = tokio::sync::watch::channel(false);
    let waiting = tokio::spawn(async move { forward(Some(sender), &serde_json::json!({"command": "git status"}), cancel, wait).await });
    let _held = calls.recv().await.unwrap();
    end.send(true).unwrap();
    assert_eq!(waiting.await.unwrap(), Err("turn_ended"));
}
