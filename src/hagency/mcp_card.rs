//! A readable card body for a Codex MCP tool call (`mcp.<server>.<tool>`).
//! The runtime hands the card `{"request": …, "item": {"type":"mcpToolCall", …}}`;
//! dumping that as JSON made the card unreadable. This names the app and
//! action, then lists every argument key with its value, long values cut, so
//! nothing that changes what the call does is hidden.
use serde_json::Value;

/// Characters of one argument value shown before it is cut.
const MAX_VALUE_CHARS: usize = 160;

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
        .or_else(|| item["tool"].as_str()).unwrap_or("tool").replace('_', " ");
    let mut lines = vec![format!("{app} · {action}")];
    lines.extend(args.iter().map(|(key, value)| format!("{}: {}", label(key), shown(value))));
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

fn label(key: &str) -> String {
    match key {
        "repository_full_name" => "repository".to_owned(),
        other => other.replace('_', " "),
    }
}

fn shown(value: &Value) -> String {
    let text = match value {
        Value::String(s) => s.split_whitespace().collect::<Vec<_>>().join(" "),
        other => other.to_string(),
    };
    if text.chars().count() <= MAX_VALUE_CHARS {
        return text;
    }
    let cut: String = text.chars().take(MAX_VALUE_CHARS).collect();
    format!("{cut}… ({} chars)", text.chars().count())
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
        assert_eq!(lines[0], "GitHub · create issue");
        assert!(lines.contains(&"repository: csheargm/hagency-demo-sandbox"), "{text}");
        assert!(lines.contains(&"title: README: add a Getting help section"), "{text}");
        assert!(lines.contains(&"body: Add help."), "{text}");
        assert!(lines.contains(&"labels: [\"docs\"]"), "{text}");
    }

    #[test]
    fn a_long_value_is_cut_and_says_how_long_it_was() {
        let args = json!({"body": "x".repeat(500)}).to_string();
        let text = summary(&input(Value::String(args))).unwrap();
        assert!(text.contains("… (500 chars)"), "{text}");
        assert!(text.lines().nth(1).unwrap().chars().count() < 200);
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
        assert!(body.starts_with("GitHub · create issue\n"), "{body}");
        assert!(body.contains("repository: o/r") && !body.contains("appContext"), "{body}");
    }
}
