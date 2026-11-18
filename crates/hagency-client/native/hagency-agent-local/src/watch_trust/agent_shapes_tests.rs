//! The shapes a Claude Agent actually wrote under push freely in the Nov 17
//! autonomous runs (issues #67-#69 of the demo sandbox), exactly as sent.
//! The pull request steps must run without a card; the chained reads keep
//! asking (one command per call is the posture, the prompt says so).
use crate::outside::shell;
use crate::watch_trust::{TrustedStep, pin_env, pinned_line};
use std::process::Command;

const REPO: &str = "csheargm/hagency-demo-sandbox";

/// #69 attempt 1, asked at 23:27:29 right after its card-free push.
const PR_CREATE_1: &str = "gh pr create --repo csheargm/hagency-demo-sandbox --draft --base main --head bird-claude/status-line-69 --title \"README: Status line\" --body \"Closes #69\n\nmain already had a Status line under the title, but with extra text. This changes it to exactly \\`Status: demo sandbox\\`, as the issue's acceptance requires.\n\nOpened by agent bird-claude (@_hagency_agt_01m7k84kgg5a5mtm2epqhbpjb5:localhost:8452) on request from issue watch for @bird:localhost:8452\"";
/// #69 attempt 3 (no `--base`, single quotes inside the double-quoted title).
const PR_CREATE_3: &str = "gh pr create --repo csheargm/hagency-demo-sandbox --draft --head fix/readme-status-69 --title \"README: Status line reads 'Status: demo sandbox'\" --body \"Closes #69\n\nThe README already had a Status line under the title; this makes it exactly \\`Status: demo sandbox\\` as the acceptance criteria require.\n\nOpened by agent bird-claude (@_hagency_agt_01m7k84kgg5a5mtm2epqhbpjb5:localhost:8452) on request from issue watch for @bird:localhost:8452\"";

#[test]
fn the_nov17_pull_request_creates_run_pinned_without_a_card() {
    let temp = tempfile::tempdir().unwrap();
    for line in [PR_CREATE_1, PR_CREATE_3] {
        assert_eq!(pinned_line(line, temp.path(), REPO), Some(TrustedStep::Command(pin_env(line))), "{line}");
    }
}

/// The reader takes a backslash inside double quotes exactly as the shell
/// does: before `` ` ``, `$`, `"` or `\` it is dropped and the next character
/// is literal. The words it checks are the words the command receives.
#[test]
fn escaped_double_quote_text_reads_as_the_shell_passes_it() {
    let line = r#"printf '%s|' "a\`id\`b" "c\$(id)d" "e\"f" "g\\h" "Closes #69""#;
    let words = &shell::parse(line).expect("readable").segments[0];
    assert_eq!(&words[2..], ["a`id`b", "c$(id)d", "e\"f", "g\\h", "Closes #69"]);
    // Claude's Bash tool runs the user's shell: zsh on macOS, else sh/bash.
    for shell in ["/bin/sh", "/bin/bash", "/bin/zsh"].into_iter().filter(|s| std::path::Path::new(s).exists()) {
        let out = Command::new(shell).arg("-c").arg(line).output().unwrap();
        let shell_words: Vec<&str> = std::str::from_utf8(&out.stdout).unwrap().split_terminator('|').collect();
        assert_eq!(&words[2..], shell_words.as_slice(), "{shell}");
    }
}

/// Anything the shell would still expand, and backslash forms the reader
/// does not need, keep the whole line asking.
#[test]
fn unescaped_expansions_and_other_backslashes_still_ask() {
    let temp = tempfile::tempdir().unwrap();
    for body in ["\"`id`\"", "\"$(id)\"", "\"$HOME\"", "\"a\\nb\"", "\"a\\\nb\"", "\"\\\"", "a\\`b", "\"\\!\""] {
        let line = format!("gh pr create --repo {REPO} --head fix/x --title t --body {body}");
        assert_eq!(pinned_line(&line, temp.path(), REPO), None, "{line}");
    }
}

/// The reads the Agent chained in the same runs (#68 attempt 1, #69 attempt
/// 2 and 3) keep asking: one command per call, no `;` and no pipe.
#[test]
fn the_nov17_chained_reads_still_ask() {
    let temp = tempfile::tempdir().unwrap();
    for line in [
        "git -C ./hagency-demo-sandbox log origin/HEAD --oneline -3 -- CODE_OF_CONDUCT.md; git -C ./hagency-demo-sandbox status -s | head; git -C ./hagency-demo-sandbox branch -r | head -30",
        "git -C hagency-demo-sandbox status -sb; git -C hagency-demo-sandbox show origin/main:README.md | head -5",
        "git -C ./hagency-demo-sandbox show origin/main:README.md --stat | head -5; git -C ./hagency-demo-sandbox show origin/main:README.md | head -5",
    ] {
        assert_eq!(pinned_line(line, temp.path(), REPO), None, "{line}");
    }
}

/// The own-watch block tells the Agent the shapes that skip the card: one
/// read per call, `--head` on create, and body text it may quote.
#[test]
fn the_watch_block_names_the_pull_request_and_one_call_rules() {
    let text = crate::watch_info::PUSH_FREELY_STEPS;
    for rule in ["--head <branch>", "| head", "single quotes"] {
        assert!(text.contains(rule), "{rule}");
    }
}
