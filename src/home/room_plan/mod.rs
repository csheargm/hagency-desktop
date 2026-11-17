//! Planner cards in the timeline (ADR 0017): an Agent's `plan_propose`
//! message drawn as its plan, with the issue titles editable and one
//! "File these issues" button for the Agent's owner (`file`).
pub mod card;
pub mod file;

use hagency::room_plan::{self as plans, Plan, PlanIssue};
use makepad_widgets::ScriptVm;

pub fn script_mod(vm: &mut ScriptVm) {
    card::script_mod(vm);
}

/// One planner card as the timeline shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanCard {
    pub id: String,
    pub plan: Plan,
    /// The Agent that posted it: the event's real sender.
    pub agent: String,
}

/// The plan an Agent's message carries, if it parses and its real sender is
/// an authentic Agent of this room's homeserver.
pub fn plan_of(event: &matrix_sdk_ui::timeline::EventTimelineItem, room: &matrix_sdk::ruma::RoomId) -> Option<PlanCard> {
    if !crate::home::room_memory::authentic::event_authentic(event, room) {
        return None;
    }
    let sender = event.sender().as_str();
    let server = hagency::agent_questions::server_name(room.as_str())?;
    if !hagency::agent_questions::is_agent_on(sender, server) {
        return None;
    }
    let raw = event.latest_json()?;
    let value: serde_json::Value = serde_json::from_str(raw.json().get()).ok()?;
    let (id, plan, _) = plans::from_content(&value["content"])?;
    Some(PlanCard { id, plan, agent: sender.to_owned() })
}

/// The line that replaces a plan message's body above its card.
pub fn intro(sender: &str) -> String {
    crate::i18n::format("{agent} proposes a plan:", &[("agent", sender.to_owned())])
}

/// The plan with the titles as edited: one title per line, in order. A kept
/// line keeps its issue's body and criteria; a new line is a new issue; a
/// removed line drops that issue.
pub fn edited(plan: &Plan, text: &str) -> Result<Plan, String> {
    let titles: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let issues = titles.iter().enumerate().map(|(n, title)| {
        let base = plan.issues.get(n).cloned().unwrap_or_default();
        PlanIssue { title: (*title).to_owned(), ..base }
    }).collect();
    Plan { issues, ..plan.clone() }.validate()
        .map_err(|_| crate::i18n::tr("Keep 1 to 12 issues, one title per line.").to_owned())
}

/// The card's editable text: one title per line.
pub fn titles(plan: &Plan) -> String {
    plan.issues.iter().map(|i| i.title.as_str()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        plans::parse(&serde_json::json!({"repo": "acme/sandbox", "title": "Docs", "issues": [
            {"title": "Add a Glossary", "acceptance": ["has a Glossary"]}, {"title": "Add a Quick start"}]})).unwrap()
    }

    #[test]
    fn edits_keep_criteria_by_position_and_add_or_drop_issues() {
        let p = plan();
        assert_eq!(edited(&p, &titles(&p)).unwrap(), p, "unchanged text, unchanged plan");
        let e = edited(&p, "Add a glossary page\n\nAdd a Quick start\nAdd an FAQ").unwrap();
        assert_eq!(e.issues.len(), 3);
        assert_eq!((e.issues[0].title.as_str(), e.issues[0].acceptance.len()), ("Add a glossary page", 1));
        assert!(e.issues[2].acceptance.is_empty());
        assert_eq!(edited(&p, "Only one").unwrap().issues.len(), 1);
        assert!(edited(&p, "  \n").is_err());
    }
}
