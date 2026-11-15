//! A live step in plain words for the run card ("running gh pr create",
//! "reading src/main.rs"). Presentation only: a step carries no authority,
//! is never persisted as task truth, and a lost step never changes the run.
//! Ported from the original Hagency demo (agent-local `progress.rs`); the
//! verbs follow [`crate::Verb::for_tool`] so the summary and the card agree.
use serde_json::Value;

/// Longest detail (file, pattern, command) shown in one step.
pub const MAX_STEP_DETAIL_CHARS: usize = 60;

/// First line, trimmed, at most [`MAX_STEP_DETAIL_CHARS`] characters.
fn short(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or("").trim();
    if line.chars().count() <= MAX_STEP_DETAIL_CHARS {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(MAX_STEP_DETAIL_CHARS - 1).collect();
    out.push('…');
    out
}
/// The last two path components; Hagency's own folders mean nothing to people.
fn file(path: &str) -> String {
    let internal = |part: &&str| {
        ["binding_", "owner_", "agt_", ".hagency"].iter().any(|p| part.starts_with(p))
    };
    let parts: Vec<&str> =
        path.trim_end_matches('/').rsplit('/').take_while(|p| !internal(p)).take(2).collect();
    short(&parts.into_iter().rev().collect::<Vec<_>>().join("/"))
}
/// Shell wrappers Codex puts around a command (`bash -lc '…'`).
fn unwrap_shell(command: &str) -> &str {
    let command = command.trim();
    for prefix in ["/bin/bash -lc ", "/bin/zsh -lc ", "bash -lc ", "zsh -lc ", "/bin/sh -c ", "sh -c "] {
        if let Some(rest) = command.strip_prefix(prefix) {
            return rest.trim().trim_matches(|c| c == '\'' || c == '"');
        }
    }
    command
}
/// Commands too small to show (they only orient the Agent).
fn trivial(words: &[&str]) -> bool {
    match words {
        [] | ["pwd" | "true" | "clear"] | ["cd", ..] => true,
        ["ls", flags @ ..] => flags.iter().all(|f| f.starts_with('-')),
        _ => false,
    }
}
/// One shell command in plain words; empty for trivial commands (not shown or counted).
pub fn command_step(command: &str) -> String {
    let command = unwrap_shell(command);
    let words: Vec<&str> = command.split_whitespace().collect();
    if trivial(&words) {
        return String::new();
    }
    format!("running {}", short(command))
}
fn text<'a>(input: &'a Value, key: &str) -> &'a str {
    input[key].as_str().unwrap_or("")
}
/// One Claude Code `tool_use` block in plain words.
pub fn claude_tool_step(name: &str, input: &Value) -> String {
    let path = || {
        let found = ["file_path", "notebook_path", "path"]
            .into_iter()
            .map(|key| text(input, key))
            .find(|p| !p.is_empty());
        found.map_or_else(|| "a file".to_owned(), file)
    };
    match name {
        "Read" => format!("reading {}", path()),
        "Edit" | "MultiEdit" | "NotebookEdit" => format!("editing {}", path()),
        "Write" => format!("writing {}", path()),
        "Grep" | "Glob" => format!("searching {}", short(text(input, "pattern"))),
        "Bash" => command_step(text(input, "command")),
        "WebFetch" => format!("fetching {}", short(text(input, "url"))),
        "WebSearch" => format!("searching the web for {}", short(text(input, "query"))),
        "TodoWrite" => "planning the next steps".to_owned(),
        other => format!("using {}", short(other.rsplit("__").next().unwrap_or(other))),
    }
}
/// The step a Codex `item/started` item begins, if it is work worth showing.
pub fn codex_item_step(item: &Value) -> Option<String> {
    match item["type"].as_str()? {
        "commandExecution" => Some(command_step(item["command"].as_str()?)),
        "fileChange" => {
            let first = item["changes"].as_array()?.first()?;
            let verb = if first["kind"] == "add" { "writing" } else { "editing" };
            Some(format!("{verb} {}", file(first["path"].as_str()?)))
        }
        "mcpToolCall" | "dynamicToolCall" => Some(format!("using {}", short(item["tool"].as_str()?))),
        "webSearch" => Some("searching the web".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn commands_read_as_plain_words_and_trivial_ones_are_skipped() {
        assert_eq!(command_step("/bin/zsh -lc 'gh pr create --fill'"), "running gh pr create --fill");
        assert_eq!(command_step("cd repo"), "");
        assert_eq!(command_step("ls -la"), "");
        assert!(command_step(&"x".repeat(200)).chars().count() <= MAX_STEP_DETAIL_CHARS + 8);
    }
    #[test]
    fn claude_and_codex_tools_name_the_file_not_the_agent_workspace() {
        let read = json!({"file_path": "/u/owner_ab/room-workspaces/binding_cd/src/main.rs"});
        assert_eq!(claude_tool_step("Read", &read), "reading src/main.rs");
        assert_eq!(claude_tool_step("mcp__hagency__room_send", &json!({})), "using room_send");
        let item = json!({"type": "fileChange", "changes": [{"kind": "add", "path": "a/b/c.md"}]});
        assert_eq!(codex_item_step(&item).as_deref(), Some("writing b/c.md"));
        assert_eq!(codex_item_step(&json!({"type": "agentMessage"})), None);
    }
}
