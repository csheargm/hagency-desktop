//! How an approval card words a command or a path so the owner can read all of
//! it. Two problems this solves:
//! - A compound command (`cargo test && git push && gh pr create ...`) was one
//!   long line; its tail (the push / pull request) fell outside the card's
//!   visible area. Each step now starts its own line; nothing is dropped.
//! - The Agent's workspace is a long absolute path
//!   (`…/owner_<hex>/room-workspaces/binding_<hex>/…`). It is shown as
//!   `~workspace/…` wherever it appears, so the part that matters is visible.

/// Shown in place of the Agent's workspace root.
pub const WORKSPACE_LABEL: &str = "~workspace";
/// Directory names that hold an Agent's per-room workspaces (hagency-agent-local `room_files`).
const WORKSPACE_PARENTS: [&str; 2] = ["room-workspaces", ".hagency-room-workspaces"];
/// A workspace directory is `binding_` followed by this many hex digits.
const BINDING_HEX_LEN: usize = 64;
/// Operators after which a new step of a compound command starts.
const STEP_SEPARATORS: [&str; 4] = ["&&", "||", ";", "|"];

/// The workspace root inside `path` (through `…/room-workspaces/binding_<hex>`), if any.
fn workspace_root(path: &str) -> Option<&str> {
    for parent in WORKSPACE_PARENTS {
        let marker = format!("/{parent}/binding_");
        let Some(at) = path.find(&marker) else { continue };
        let hex_start = at + marker.len();
        let hex = path.get(hex_start..hex_start + BINDING_HEX_LEN)?;
        if hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return path.get(path_start(path, at)..hex_start + BINDING_HEX_LEN);
        }
    }
    None
}

/// Where the absolute path containing byte `at` starts: the nearest `/` at
/// the start of the text or after a space, quote, `=` or `(`. Directory names
/// may contain spaces ("Application Support"), but none starts with `/`.
fn path_start(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    (0..=at).rev()
        .find(|&i| bytes[i] == b'/' && (i == 0 || matches!(bytes[i - 1], b' ' | b'\t' | b'"' | b'\'' | b'=' | b'(')))
        .unwrap_or(0)
}

/// `path` with the Agent's workspace root shown as `~workspace`; other paths unchanged.
pub fn relative_to_workspace(path: &str) -> String {
    match workspace_root(path) {
        Some(root) => replace_root(path, root),
        None => path.to_owned(),
    }
}

fn replace_root(text: &str, root: &str) -> String {
    text.replace(root, WORKSPACE_LABEL)
}

/// The full command for a card: each step of a compound command on its own
/// line, and the workspace root (found in `cwd` or in the command) shortened.
/// Every character of the command except separating spaces is kept.
pub fn readable_command(command: &str, cwd: Option<&str>) -> String {
    let root = cwd.and_then(workspace_root).or_else(|| workspace_root(command));
    let shortened = match root {
        Some(root) => replace_root(command, root),
        None => command.to_owned(),
    };
    split_steps(&shortened)
}

/// Breaks the line after each unquoted step separator.
fn split_steps(command: &str) -> String {
    let mut out = String::with_capacity(command.len() + 8);
    let (mut quote, mut escaped) = (None::<char>, false);
    let mut rest = command;
    while let Some(c) = rest.chars().next() {
        if quote.is_none() && !escaped
            && let Some(sep) = STEP_SEPARATORS.iter().find(|s| rest.starts_with(**s))
        {
            out.push_str(sep);
            out.push('\n');
            rest = rest[sep.len()..].trim_start_matches(' ');
            continue;
        }
        match c {
            '\\' if quote != Some('\'') => escaped = !escaped,
            '\'' | '"' if !escaped => {
                quote = match quote {
                    None => Some(c),
                    Some(q) if q == c => None,
                    other => other,
                };
                escaped = false;
            }
            _ => escaped = false,
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> String {
        format!("/Users/ann/Library/Application Support/Hagency/owner_{}/room-workspaces/binding_{}", "a".repeat(64), "b".repeat(64))
    }

    /// The owner's flow from the demo: fix, test, push and open a pull request in one command.
    #[test]
    fn a_compound_command_keeps_its_push_and_pull_request_tail() {
        let command = "cargo test && git commit -am 'fix: a && b' && git push -u origin fix/6 && gh pr create --draft --fill";
        let shown = readable_command(command, None);
        assert_eq!(shown.lines().collect::<Vec<_>>(), [
            "cargo test &&", "git commit -am 'fix: a && b' &&", "git push -u origin fix/6 &&", "gh pr create --draft --fill"]);
        assert_eq!(shown.replace('\n', " "), command, "nothing is dropped");
    }

    #[test]
    fn pipes_and_semicolons_split_but_quoted_ones_do_not() {
        assert_eq!(readable_command("ls | wc -l; echo \"a;b\"", None), "ls |\nwc -l;\necho \"a;b\"");
        assert_eq!(readable_command(r"echo a\;b", None), r"echo a\;b");
    }

    #[test]
    fn the_workspace_root_reads_as_tilde_workspace() {
        let cwd = format!("{}/repo", ws());
        assert_eq!(relative_to_workspace(&cwd), "~workspace/repo");
        assert_eq!(relative_to_workspace(&ws()), "~workspace");
        let command = format!("cat {}/notes.txt", ws());
        assert_eq!(readable_command(&command, Some(&cwd)), "cat ~workspace/notes.txt");
        assert_eq!(readable_command(&command, None), "cat ~workspace/notes.txt");
    }

    #[test]
    fn other_paths_are_left_alone() {
        assert_eq!(relative_to_workspace("/etc/hosts"), "/etc/hosts");
        let short = "/x/room-workspaces/binding_abc/y";
        assert_eq!(relative_to_workspace(short), short, "not a real binding directory");
        let dotted = format!("/w/.hagency-room-workspaces/binding_{}/f", "c".repeat(64));
        assert_eq!(relative_to_workspace(&dotted), "~workspace/f");
    }
}
