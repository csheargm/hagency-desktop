//! A readable card body for a Codex MCP tool call (`mcp.<server>.<tool>`).
//! The runtime hands the card `{"request": …, "item": {"type":"mcpToolCall", …}}`;
//! dumping that as JSON made the card unreadable. This names the app and
//! action, then lists every argument as `key: value`, one per line, in full
//! (the card scrolls). Values are JSON, so `"false"` and `false` differ and
//! whitespace is visible; every untrusted field is escaped, so no key or value
//! can start a fake line or reorder text with invisible controls.
use serde_json::Value;

/// The summary lines, or None when `input` is not a Codex MCP tool call whose
/// arguments parse as a JSON object (the caller then shows the raw JSON).
pub fn summary(input: &Value) -> Option<String> {
    let item = &input["item"];
    if item["type"] != "mcpToolCall" {
        return None;
    }
    let args = arguments(&item["arguments"])?;
    let app = item["appContext"]["appName"].as_str()
        .or_else(|| item["server"].as_str()).unwrap_or("MCP");
    let action = item["appContext"]["actionName"].as_str()
        .or_else(|| item["tool"].as_str()).unwrap_or("tool");
    let mut lines = vec![format!("{} · {}", plain(app), plain(action))];
    lines.extend(args.iter().map(|(key, value)| format!("{}: {}", plain(key), plain(&value.to_string()))));
    Some(lines.join("\n"))
}

/// Codex sends the arguments as a JSON string; an object is accepted too.
fn arguments(raw: &Value) -> Option<serde_json::Map<String, Value>> {
    match raw {
        Value::String(text) => match serde_json::from_str(text) {
            Ok(Value::Object(map)) => Some(map),
            _ => None,
        },
        Value::Object(map) => Some(map.clone()),
        _ => None,
    }
}

/// `text` with control, bidi and invisible format characters shown as `\u{…}`.
fn plain(text: &str) -> String {
    text.chars().map(|c| if hidden(c) { format!("\\u{{{:x}}}", c as u32) } else { c.to_string() }).collect()
}
fn hidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(arguments: Value) -> Value {
        json!({"request": {}, "item": {"type": "mcpToolCall", "server": "codex_apps",
            "tool": "github.create_issue",
            "appContext": {"actionName": "create_issue", "appName": "GitHub"},
            "arguments": arguments}})
    }

    #[test]
    fn a_github_issue_call_reads_as_app_action_and_every_argument() {
        let args = json!({"body": "Add help.", "repository_full_name": "csheargm/hagency-demo-sandbox",
            "title": "README: add a Getting help section", "labels": ["docs"]}).to_string();
        let text = summary(&input(Value::String(args))).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "GitHub · create_issue");
        assert!(lines.contains(&"repository_full_name: \"csheargm/hagency-demo-sandbox\""), "{text}");
        assert!(lines.contains(&"title: \"README: add a Getting help section\""), "{text}");
        assert!(lines.contains(&"body: \"Add help.\""), "{text}");
        assert!(lines.contains(&"labels: [\"docs\"]"), "{text}");
    }

    /// GPT review: nothing is cut, types and whitespace stay visible, and
    /// no key or value can forge a line or hide behind bidi controls.
    #[test]
    fn values_are_lossless_and_cannot_forge_lines() {
        let args = json!({"body": "x".repeat(500), "dry_run": "false", "force": false,
            "a\nrepository: trusted/repo": "b\u{202e}c  d\ne"}).to_string();
        let text = summary(&input(Value::String(args))).unwrap();
        assert!(text.contains(&"x".repeat(500)), "nothing is cut");
        assert!(text.contains("dry_run: \"false\"") && text.contains("force: false"), "{text}");
        assert_eq!(text.lines().count(), 5, "{text}");
        assert!(text.contains("a\\u{a}repository: trusted/repo: \"b\\u{202e}c  d\\ne\""), "{text}");
    }

    #[test]
    fn unparseable_arguments_or_another_item_fall_back() {
        assert_eq!(summary(&input(json!("not json"))), None);
        assert_eq!(summary(&json!({"item": {"type": "commandExecution"}})), None);
    }

    /// The user flow: a Codex runtime request becomes a room card.
    #[test]
    fn a_codex_mcp_request_shows_a_readable_room_card() {
        let args = json!({"repository_full_name": "o/r", "title": "T"}).to_string();
        let view = json!({"proposalId": "p", "argsDigest": "d", "proposal": {
            "scope": {"agent": "a", "room": "!r:s", "thread": null},
            "tool": "mcp.codex_apps.github.create_issue", "arguments": input(Value::String(args)),
            "canonical_directory": "/w", "expires": 1}});
        let row = crate::hagency::local_approvals::to_row(&view, "betty-codex").unwrap();
        let (title, body) = crate::hagency::pending_approvals::describe(&row);
        assert!(title.contains("betty-codex wants to use"), "{title}");
        assert!(body.starts_with("GitHub · create_issue\n"), "{body}");
        assert!(body.contains("repository_full_name: \"o/r\"") && !body.contains("appContext"), "{body}");
    }
}
