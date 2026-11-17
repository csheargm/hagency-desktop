//! "File these issues" (ADR 0017): run on the Agent owner's device. The card's
//! sender must be one of this account's own Agents (looked up by its Matrix
//! ID, never taken from the card), whose GitHub token files the issues. The
//! plan is then recorded through the owner API so the poller can say "Done"
//! when every issue is closed as completed, and the room is told what was filed.
use crate::hagency::backend::Service;
use crate::hagency::issue_watch::{active_agents, github_token, live};
use crate::hagency::model::Command;
use ::hagency::issue_watch::{Client, GITHUB_API, create_issue};
use ::hagency::room_plan::Plan;
use matrix_sdk::ruma::{OwnedRoomId, OwnedTransactionId, events::room::message::RoomMessageEventContent};
use serde_json::{Value, json};

/// The Agent id of the owned, active Agent whose Matrix ID is `sender`.
pub fn owned_agent(listed: &Value, sender: &str) -> Option<String> {
    active_agents(listed).ok()?.into_iter().find(|a| a["puppetMxid"] == sender).and_then(|a| a["id"].as_str().map(str::to_owned))
}

/// Files `plan` (card `id`, posted by `sender` in `room`); answers the numbers.
pub async fn file(room: OwnedRoomId, sender: String, id: String, plan: Plan) -> Result<Vec<u64>, String> {
    let service = Service::for_current()?;
    let listed = service.execute(Command::Agents).await?;
    let agent = owned_agent(&listed, &sender).ok_or_else(|| crate::i18n::tr("Only this Agent's owner can file its plan, from the device that runs it.").to_owned())?;
    if crate::hagency::kill_switch::paused_here(&agent) {
        return Err(crate::i18n::tr("Agent paused by the owner").to_owned());
    }
    let token = github_token(&agent).await?;
    // Reserved before anything is written on GitHub, so a card is filed once:
    // a done plan refuses, and an interrupted one resumes its missing steps.
    let reserve = json!({"id": id, "repo": plan.repo, "roomId": room.as_str(), "issues": vec![0u64; plan.issues.len()]});
    let reply = live::saved(&service, &agent, Some(json!({"plan": {"filed": reserve}}))).await?;
    let mut numbers = stored_numbers(&reply, plan.issues.len())?;
    let client = Client::new();
    for (index, issue) in plan.issues.iter().enumerate() {
        if numbers[index] != 0 {
            continue;
        }
        // Stop All between steps holds the rest (GPT review).
        if crate::hagency::kill_switch::paused_here(&agent) {
            return Err(failed(index, plan.issues.len(), crate::i18n::tr("Agent paused by the owner")));
        }
        let number = create_issue(&client, GITHUB_API, &plan.repo, &token, &issue.title, &plan.issue_body(index, &id)).await
            .map_err(|error| failed(index, plan.issues.len(), &::hagency::github_access::redact(&error.to_string(), &token)))?;
        // Saved at once: a crash after this step never files it again.
        live::saved(&service, &agent, Some(json!({"plan": {"issue": {"id": id, "index": index, "number": number}}}))).await
            .map_err(|error| failed(index + 1, plan.issues.len(), &error))?;
        numbers[index] = number;
    }
    let list: Vec<String> = numbers.iter().zip(&plan.issues).map(|(n, i)| format!("#{n} {}", i.title)).collect();
    if crate::hagency::kill_switch::paused_here(&agent) {
        return Err(crate::i18n::tr("Agent paused by the owner").to_owned());
    }
    let text = crate::i18n::format("Filed on {repo}: {issues}", &[("repo", plan.repo.clone()), ("issues", list.join(", "))]);
    crate::hagency::room_send::send_routed_with(room, RoomMessageEventContent::notice_plain(text), Default::default(), filed_txn_id(&id)).await?;
    Ok(numbers)
}

fn failed(count: usize, total: usize, error: &str) -> String {
    crate::i18n::format("Filed {count} of {total}; then GitHub refused: {error}", &[("count", count.to_string()), ("total", total.to_string()), ("error", error.to_owned())])
}

/// The reserved plan's numbers from the owner API reply; a done plan or one
/// reserved with another number of steps is refused.
pub fn stored_numbers(reply: &Value, steps: usize) -> Result<Vec<u64>, String> {
    if reply["plan"]["done"] == true {
        return Err(crate::i18n::tr("These issues were already filed.").to_owned());
    }
    let numbers: Vec<u64> = serde_json::from_value(reply["plan"]["plan"]["issues"].clone()).map_err(|_| crate::i18n::tr("Unreadable plan record").to_owned())?;
    if numbers.len() != steps {
        return Err(crate::i18n::tr("This plan was filed earlier with a different number of issues.").to_owned());
    }
    Ok(numbers)
}

fn filed_txn_id(plan: &str) -> OwnedTransactionId {
    OwnedTransactionId::from(format!("hagency-plan-filed-{}", blake3::hash(plan.as_bytes()).to_hex()))
}

#[cfg(test)]
mod tests {
    use super::{owned_agent, stored_numbers};
    use serde_json::json;

    #[test]
    fn only_an_active_agent_of_this_account_with_that_matrix_id_files() {
        let listed = json!({"agents": [
            {"id": "agt_a", "state": "active", "puppetMxid": "@anna-claude:x"},
            {"id": "agt_b", "state": "paused", "puppetMxid": "@anna-codex:x"}]});
        assert_eq!(owned_agent(&listed, "@anna-claude:x").as_deref(), Some("agt_a"));
        assert_eq!(owned_agent(&listed, "@anna-codex:x"), None);
        assert_eq!(owned_agent(&listed, "@betty-claude:x"), None, "someone else's Agent");
    }

    #[test]
    fn a_reserved_plan_resumes_its_missing_steps_and_a_done_one_refuses() {
        let reply = |issues: serde_json::Value, done: bool| json!({"plan": {"plan": {"issues": issues}, "done": done}});
        assert_eq!(stored_numbers(&reply(json!([12, 0]), false), 2).unwrap(), [12, 0]);
        assert!(stored_numbers(&reply(json!([12, 13]), true), 2).is_err(), "done: never filed again");
        assert!(stored_numbers(&reply(json!([12]), false), 2).is_err());
        assert!(stored_numbers(&json!({"plan": null}), 2).is_err());
    }
}
