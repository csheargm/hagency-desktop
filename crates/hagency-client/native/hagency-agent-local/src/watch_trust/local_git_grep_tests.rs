//! `git grep` in the own checkout runs without a card (#112 hand-off, Nov 18
//! 20:13: bird-claude's read asked its owner); nothing that runs a program,
//! reads outside the checkout or hides a path behind a pattern is allowed.
use super::{REPO, command, workspace};
use crate::watch_trust::pinned_line;

#[test]
fn a_read_only_grep_is_pinned() {
    let (_t, root) = workspace();
    for line in [
        "git -C ./app grep -n -i 'changelog' origin/main -- README.md",
        "git -C app grep -n hi",
        "git -C app grep -c -e hi -e '-x' HEAD",
        "git -C app grep -A3 --context=2 -B 1 -w hi -- README.md",
        "git -C app grep --cached -l hi",
    ] {
        let pinned = command(line, &root).unwrap_or_else(|| panic!("{line}"));
        assert!(pinned.contains("'grep' '--no-textconv'") && pinned.contains("'core.pager=cat'"), "{line}: {pinned}");
    }
    let out = std::process::Command::new("sh").arg("-c").arg(command("git -C app grep -n hi -- README.md", &root).unwrap()).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "README.md:1:hi\n");
}

#[test]
fn a_grep_that_runs_a_program_or_reads_outside_asks() {
    let (_t, root) = workspace();
    for line in [
        "git -C app grep -O hi",
        "git -C app grep -Ovim hi",
        "git -C app grep --open-files-in-pager=sh hi",
        "git -C app grep --open-files-in-pager hi",
        "git -C app grep --no-index hi /etc",
        "git -C app grep hi /etc/passwd",
        "git -C app grep hi -- ../outside",
        "git -C app grep -f /etc/passwd",
        "git -C app grep --textconv hi",
        "git -C app grep --recurse-submodules hi",
        "git -C app grep -A x hi",
        "git -C app grep /etc -e hi",
        "git -C app grep",
        "git -C app grep -- README.md",
        "git -C app grep hi | sh",
    ] {
        assert_eq!(pinned_line(line, &root, REPO), None, "{line}");
    }
}
