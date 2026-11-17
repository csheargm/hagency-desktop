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

/// The plan with the titles as edited: one line per issue, in order. Only
/// titles change; each keeps its own body and criteria, so the count must stay.
pub fn edited(plan: &Plan, text: &str) -> Result<Plan, String> {
    let titles: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if titles.len() != plan.issues.len() {
        return Err(crate::i18n::format("Keep one title per line for each of the {count} issues.", &[("count", plan.issues.len().to_string())]));
    }
    let issues = plan.issues.iter().zip(titles).map(|(issue, title)| PlanIssue { title: title.to_owned(), ..issue.clone() }).collect();
    Plan { issues, ..plan.clone() }.validate().map_err(|_| crate::i18n::tr("Each title must be one line of at most 200 characters.").to_owned())
}

/// Everything filing publishes besides the titles: each issue's body and
/// criteria, shown in full so the owner approves what is actually sent.
pub fn details(plan: &Plan) -> String {
    plan.issues.iter().enumerate().map(|(n, issue)| {
        let mut text = format!("{}. {}", n + 1, issue.title);
        if !issue.body.trim().is_empty() {
            text.push_str(&format!("\n{}", issue.body.trim()));
        }
        for item in &issue.acceptance {
            text.push_str(&format!("\n  ☐ {item}"));
        }
        text
    }).collect::<Vec<_>>().join("\n\n")
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
    fn edits_rename_in_place_and_never_move_a_body_to_another_issue() {
        let p = plan();
        assert_eq!(edited(&p, &titles(&p)).unwrap(), p, "unchanged text, unchanged plan");
        let e = edited(&p, "Add a glossary page\n\nAdd a Quick start").unwrap();
        assert_eq!((e.issues[0].title.as_str(), e.issues[0].acceptance.len()), ("Add a glossary page", 1));
        assert!(edited(&p, "Add a Quick start").is_err(), "dropping a line would shift bodies");
        assert!(edited(&p, "a\nb\nc").is_err());
    }

    #[test]
    fn the_card_shows_every_body_and_criterion_that_filing_publishes() {
        let text = details(&plan());
        assert!(text.contains("1. Add a Glossary") && text.contains("☐ has a Glossary") && text.contains("2. Add a Quick start"));
    }
}
