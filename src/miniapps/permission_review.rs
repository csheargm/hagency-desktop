//! Presentation only: group every requested capability without changing grants.
use crate::i18n::tr;

pub(super) const COUNT: usize = 6;
const TITLES: [&str; COUNT] = [
    "Accounts & identity", "Resources & budgets", "Agents & execution",
    "Approvals & access", "Files & workspaces", "Additional permissions",
];
const SUMMARIES: [&str; COUNT] = [
    "Use your Matrix identity and connect or retire provider accounts.",
    "Review, configure, publish and renew resource pools, grants and Agent budgets.",
    "Request and manage Agents, tasks, execution and recovery.",
    "Review assigned requests and manage approvals, tool permissions and access rules.",
    "Manage workspace status and rules, local drafts, and connection-file imports or downloads.",
    "Review additional requested capabilities.",
];
pub(super) struct Group {
    pub title: &'static str,
    pub summary: &'static str,
    pub capabilities: Vec<String>,
}
fn category(cap: &str) -> usize {
    if cap == "storage" || cap.starts_with("hagency.workspaces.")
        || cap.starts_with("hagency.personal_agents.workspace.") || cap == "hagency.personal_agents.rules.edit"
        || matches!(cap, "hagency.web.terminal" | "hagency.web_connections.import" | "hagency.web_connections.replace" | "hagency.web_connections.download") { return 4; }
    if cap.starts_with("hagency.inbox.") || cap.contains("approvals.") || cap.contains("approval_bindings.")
        || cap.starts_with("hagency.tool_grants.") || cap.starts_with("hagency.execution_policy.")
        || cap.ends_with(".decide") || cap.ends_with(".review") || cap.ends_with(".reconcile") { return 3; }
    if cap == "hagency.session" || ["hagency.accounts.", "hagency.account.", "hagency.providers.", "hagency.people.",
        "hagency.onboarding.", "hagency.connections.", "hagency.web_connections."].iter().any(|p|cap.starts_with(p))
        || cap == "hagency.web.accounts" { return 0; }
    if ["hagency.resources.", "hagency.grants.", "hagency.pools.", "hagency.alerts.", "hagency.web_catalog.",
        "hagency.agents.budget.", "hagency.agents.topups."].iter().any(|p|cap.starts_with(p))
        || matches!(cap, "hagency.web.capability" | "hagency.web.usage" | "hagency.web.alerts") { return 1; }
    if ["hagency.agents.", "hagency.personal_agents.", "hagency.execution.", "hagency.graphs.", "hagency.graph_history.",
        "hagency.tasks.", "hagency.templates.", "hagency.stopped.", "hagency.intent.", "hagency.navigation."].iter().any(|p|cap.starts_with(p))
        || cap == "hagency.web.workforce" { return 2; }
    5
}
pub(super) fn groups(capabilities: &[String]) -> [Group; COUNT] {
    let mut groups = std::array::from_fn(|i| Group {title:TITLES[i], summary:SUMMARIES[i], capabilities:Vec::new()});
    for cap in capabilities { groups[category(cap)].capabilities.push(cap.clone()); }
    groups
}
pub(super) fn details(group: &Group) -> String {
    group.capabilities.iter().map(|cap| {
        let words = if cap == "storage" {Some("Store this app’s data on this device.")}
            else {octosense_app_contract::hagency::words(cap)};
        match words {
            Some(words) => format!("• {}\n  {}", tr(words), cap),
            None => format!("• {cap}"),
        }
    }).collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_shipped_permission_including_storage_is_reviewable_exactly_once() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!("../../apps/hagency/bundle/manifest.json")).unwrap();
        let mut requested = manifest["capabilities"].as_array().unwrap().iter().map(|v|v.as_str().unwrap().to_owned()).collect::<Vec<_>>();
        let groups = groups(&requested);
        let mut shown = groups.iter().flat_map(|g|g.capabilities.clone()).collect::<Vec<_>>();
        requested.sort(); shown.sort();
        assert_eq!(shown, requested);
        assert!(groups[4].capabilities.iter().any(|cap|cap == "storage"));
        for group in groups {
            let text = details(&group);
            for cap in group.capabilities { assert!(text.contains(&cap), "{cap}"); }
        }
    }
    #[test]
    fn unfamiliar_permissions_remain_visible_in_additional_details() {
        let groups = groups(&["hagency.future.permission".into()]);
        assert_eq!(groups[5].capabilities, ["hagency.future.permission"]);
        assert!(details(&groups[5]).contains("hagency.future.permission"));
    }
}
