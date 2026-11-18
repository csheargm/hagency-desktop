//! User flow (live 2026-11-18 09:17): Betty's bird-claude, watching
//! csheargm/hagency-demo-sandbox with "push freely" on, is handed #96, finds
//! nothing to change and posts the hand-off's cannot-fix comment. That one
//! command now runs without a card (Claude `Bash` and Codex's step tool);
//! any other comment, repository, issue, option or shell trick still asks.
use super::CannotFix;
use crate::ToolPolicy;
use crate::issue_track::{CANNOT_FIX_MARKER, TrackChange, cannot_fix_step};
use crate::watch_trust::TrustedStep;
use crate::watch_trust::fix_flow_tests::{OWNER, REPO, ROOM, Run, proposal, run_under};
use serde_json::json;

/// The command bird-claude ran, word for word.
const LIVE: &str = "gh issue comment 96 --repo csheargm/hagency-demo-sandbox --body '🤖 Hagency: cannot fix - nothing to change. … Commented by agent bird-claude (@_hagency_agt_…)'";
const LIVE_BODY: &str = "🤖 Hagency: cannot fix - nothing to change. … Commented by agent bird-claude (@_hagency_agt_…)";

/// `line` read as the cannot-fix comment: number, repository, body, and
/// the command that runs in its place.
fn parse(line: &str) -> Option<(u64, String, String, String)> {
    let parsed = crate::outside::shell::parse(line)?;
    let [words] = parsed.segments.as_slice() else { return None };
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let c = CannotFix::parse(line, &words)?;
    Some((c.number, c.repo.to_owned(), c.body.to_owned(), c.command()))
}

/// The watch with push freely on, `handed` already handed to bird.
fn handed(handed: &[i64], policy: ToolPolicy) -> Run {
    let mut r = run_under(policy);
    for number in handed {
        let change: TrackChange = serde_json::from_value(json!({"repo": REPO, "number": number, "handed": 1})).unwrap();
        r.ledger.track_issue("bird", &change, 1).unwrap();
    }
    r
}

fn answer(r: &mut Run, tool: &str, line: &str) -> Option<TrustedStep> {
    r.ledger.trusted_publish(OWNER, &proposal(&r.root, ROOM, tool, json!({"command": line})), 11).unwrap()
}

#[test]
fn the_live_command_parses_and_the_hand_off_states_its_shape() {
    let (number, repo, body, _) = parse(LIVE).unwrap();
    assert_eq!((number, repo.as_str(), body.as_str()), (96, REPO, LIVE_BODY));
    let (number, _, body, _) = parse("gh issue comment 96 -b '🤖 Hagency: cannot fix - no change' -R csheargm/hagency-demo-sandbox").unwrap();
    assert_eq!((number, body.as_str()), (96, "🤖 Hagency: cannot fix - no change"));
    let stated = cannot_fix_step("96", REPO);
    assert!(stated.starts_with(&format!("gh issue comment 96 --repo {REPO} --body '{CANNOT_FIX_MARKER} - ")) && stated.ends_with('\''), "{stated}");
}

#[test]
fn anything_else_is_not_the_cannot_fix_comment() {
    let body = "'🤖 Hagency: cannot fix - x'";
    for line in [
        format!("gh issue comment 96 --repo {REPO} --body 'Hagency: cannot fix - x'"),
        format!("gh issue comment 96 --repo {REPO} --body 'I cannot fix it. 🤖 Hagency: cannot fix'"),
        format!("gh issue comment 96 --repo {REPO} --body \"🤖 Hagency: cannot fix - x\""),
        format!("gh issue comment 96 --repo {REPO} --body {body} --edit-last"),
        format!("gh issue comment 96 --repo {REPO} --body {body} -F notes.md"),
        format!("gh issue comment 96 --repo {REPO} --body {body} --web"),
        format!("gh issue comment 96 --repo {REPO} --body {body} --body {body}"),
        format!("gh issue comment 96 --repo {REPO} --repo other/repo --body {body}"),
        format!("gh issue comment 96 --repo={REPO} --body {body}"),
        format!("gh issue comment 96 --body {body}"),
        format!("gh issue comment --repo {REPO} --body {body}"),
        format!("gh issue comment 0 --repo {REPO} --body {body}"),
        format!("gh issue comment +96 --repo {REPO} --body {body}"),
        format!("gh issue comment 2147483648 --repo {REPO} --body {body}"),
        format!("gh issue comment https://github.com/{REPO}/issues/96 --repo {REPO} --body {body}"),
        format!("gh issue comment 96 --repo github.example.com/{REPO} --body {body}"),
        format!("gh issue comment 96 --repo {REPO} --body {body} 2>&1"),
        format!("gh issue comment 96 --repo {REPO} --body {body}; gh issue close 96"),
        format!("gh issue comment 96 --repo {REPO} --body {body} && echo ok"),
        format!("gh issue comment 96 --repo {REPO} --body {body} | cat"),
        format!("gh issue comment 96 --repo {REPO} --body '🤖 Hagency: cannot fix - $(cat ~/.ssh/id_rsa)'"),
        format!("gh issue comment 96 --repo {REPO} --body '🤖 Hagency: cannot fix - `id`'"),
        format!("gh issue comment 96 --repo {REPO} --body \"🤖 Hagency: cannot fix - $(id)\""),
        format!("gh issue comment 96 --repo {REPO} --body '🤖 Hagency: cannot fix - a\nb'"),
        format!("gh issue comment 96 --repo {REPO} --body '🤖 Hagency: cannot fix - it'\\''s done'"),
        format!("GH_HOST=evil gh issue comment 96 --repo {REPO} --body {body}"),
        format!("gh issue close 96 --repo {REPO} --comment {body}"),
        format!("gh issue edit 96 --repo {REPO} --body {body}"),
        format!("gh pr comment 96 --repo {REPO} --body {body}"),
    ] {
        assert_eq!(parse(&line), None, "{line}");
    }
    let long = format!("gh issue comment 96 --repo {REPO} --body '{CANNOT_FIX_MARKER} - {}'", "x".repeat(1000));
    assert_eq!(parse(&long), None, "too long");
}

#[test]
fn what_runs_is_rebuilt_quoted_and_pinned() {
    let command = parse(LIVE).unwrap().3;
    assert!(command.starts_with("GIT_CONFIG_COUNT="), "{command}");
    assert!(command.ends_with(&format!("GH_HOST='github.com' gh issue comment 96 --repo '{REPO}' --body '{LIVE_BODY}'")), "{command}");
    assert!(crate::watch_trust::step_env::runs_gh(&command), "the step gets gh's token like the other gh steps");
    let echoed = std::process::Command::new("sh").arg("-c").arg(format!("printf %s {}", super::super::quote(LIVE_BODY))).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&echoed.stdout), LIVE_BODY);
}

#[test]
fn a_handed_issue_gives_up_without_a_card_for_claude_and_codex() {
    for policy in [ToolPolicy::AskOwner, ToolPolicy::Deny] {
        let mut r = handed(&[96], policy);
        let expected = parse(LIVE).unwrap().3;
        assert_eq!(answer(&mut r, "claude.Bash", LIVE), Some(TrustedStep::Command(expected.clone())));
        assert_eq!(answer(&mut r, super::super::codex_step::TOOL, LIVE), Some(TrustedStep::Command(expected)));
        let other_case = LIVE.replace(REPO, "CSheargm/Hagency-Demo-Sandbox");
        assert!(answer(&mut r, "claude.Bash", &other_case).is_some(), "GitHub names ignore case");
    }
}

#[test]
fn other_issues_repos_requesters_and_closed_tracks_still_ask() {
    let mut r = handed(&[96], ToolPolicy::AskOwner);
    for line in [
        LIVE.replace("comment 96", "comment 97"),
        LIVE.replace(REPO, "csheargm/other-repo"),
        format!("gh issue comment 96 --repo {REPO} --body 'Thanks for the report!'"),
        format!("gh issue close 96 --repo {REPO}"),
    ] {
        assert_eq!(answer(&mut r, "claude.Bash", &line), None, "{line}");
    }
    let mut stranger = proposal(&r.root, ROOM, "claude.Bash", json!({"command": LIVE}));
    stranger.scope.requester = "@mallory:example.org".into();
    assert_eq!(r.ledger.trusted_publish(OWNER, &stranger, 11).unwrap(), None, "another requester asks");
    let background = proposal(&r.root, ROOM, "claude.Bash", json!({"command": LIVE, "run_in_background": true}));
    assert_eq!(r.ledger.trusted_publish(OWNER, &background, 11).unwrap(), None, "a background call asks");
    let gave_up: TrackChange = serde_json::from_value(json!({"repo": REPO, "number": 96, "state": "needs_human"})).unwrap();
    r.ledger.track_issue("bird", &gave_up, 2).unwrap();
    assert_eq!(answer(&mut r, "claude.Bash", LIVE), None, "a finished track asks");
}

#[test]
fn without_push_freely_or_a_hand_off_it_asks() {
    let mut r = handed(&[], ToolPolicy::AskOwner);
    assert_eq!(answer(&mut r, "claude.Bash", LIVE), None, "never handed");
    let mut r = handed(&[96], ToolPolicy::AskOwner);
    let mut setting = r.ledger.issue_watch("bird").unwrap().setting;
    setting.push_trusted = false;
    r.ledger.set_issue_watch(OWNER, "bird", setting, 1).unwrap();
    assert_eq!(answer(&mut r, "claude.Bash", LIVE), None, "push freely off");
}
