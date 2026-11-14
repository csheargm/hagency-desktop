//! What the issue-watch controls show and save, without widgets: the mode
//! choices, their hints, the room list and the `{issueWatch}` change.
use serde_json::{Value, json};

/// Issue-watch modes (the engine's `WatchMode` wire names) in dropdown order.
pub(crate) const MODES: [&str; 4] = ["off", "labelled", "trusted", "all"];
/// The dropdown labels, in `MODES` order (English keys; translate on use).
pub(crate) const MODE_LABELS: [&str; 4] = [
    "Off",
    "Labelled issues (agent-fix)",
    "Trusted team: issues from the repository's owner, members and collaborators",
    "Fully automatic: every new issue",
];

/// The one-line explanation under the mode choice.
pub(crate) fn hint(index: usize) -> &'static str {
    match MODES.get(index).copied() {
        Some("labelled") => "Hands off open issues that carry the agent-fix label.",
        Some("trusted") => "Hands off new issues opened by the repository's owner, members and collaborators.",
        Some("all") => "Hands off every new issue, from anyone. Use only on repositories you trust.",
        _ => "The Agent does not watch GitHub issues.",
    }
}
/// The dropdown position of a saved mode; never set means Off.
pub(crate) fn index(mode: &str) -> usize {
    MODES.iter().position(|m| *m == mode).unwrap_or(0)
}
/// The rooms the Agent can be asked in: `(roomId, name)` of its active bindings.
pub(crate) fn rooms(bindings: &[Value], mut name: impl FnMut(&Value) -> String) -> Vec<(String, String)> {
    bindings
        .iter()
        .filter(|b| b["state"] == "active")
        .filter_map(|b| {
            let id = b["roomId"].as_str().filter(|id| id.starts_with('!'))?;
            Some((id.to_owned(), name(b)))
        })
        .collect()
}
/// The `{issueWatch}` change to save. Off needs nothing else; the others
/// need an `owner/name` repository and one of the Agent's rooms. The engine
/// checks both again before it stores anything.
pub(crate) fn change(index: usize, repo: &str, room_id: Option<&str>) -> Result<Value, &'static str> {
    let mode = MODES.get(index).copied().unwrap_or("off");
    let repo = repo.trim();
    let room_id = room_id.unwrap_or("");
    if mode != "off" {
        let part = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !repo.split_once('/').is_some_and(|(owner, name)| part(owner) && part(name)) {
            return Err("Enter the repository as owner/name.");
        }
        if room_id.is_empty() {
            return Err("Connect this Agent to a Room first, then choose it here.");
        }
    }
    let label = ::hagency::issue_watch::DEFAULT_LABEL;
    Ok(json!({"issueWatch": {"mode": mode, "label": label, "repo": repo, "roomId": room_id}}))
}
