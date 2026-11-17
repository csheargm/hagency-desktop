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
    let saved = live::saved(&service, &agent, None).await?;
    if crate::hagency::issue_watch::plans::open_plans(&saved).iter().any(|p| p.id == id) {
        return Err(crate::i18n::tr("These issues were already filed.").to_owned());
    }
    if crate::hagency::kill_switch::paused_here(&agent) {
        return Err(crate::i18n::tr("Agent paused by the owner").to_owned());
    }
    let token = github_token(&agent).await?;
    let client = Client::new();
    let mut numbers = Vec::new();
    let mut failure = None;
    for (index, issue) in plan.issues.iter().enumerate() {
        match create_issue(&client, GITHUB_API, &plan.repo, &token, &issue.title, &plan.issue_body(index, &id)).await {
            Ok(number) => numbers.push(number),
            Err(error) => { failure = Some(::hagency::github_access::redact(&error.to_string(), &token)); break; }
        }
    }
    if !numbers.is_empty() {
        // What was filed is recorded and announced even when a later issue failed.
        let record = json!({"id": id, "repo": plan.repo, "roomId": room.as_str(), "issues": numbers});
        live::saved(&service, &agent, Some(json!({"plan": {"filed": record}}))).await?;
        let list: Vec<String> = numbers.iter().zip(&plan.issues).map(|(n, i)| format!("#{n} {}", i.title)).collect();
        let text = crate::i18n::format("Filed on {repo}: {issues}", &[("repo", plan.repo.clone()), ("issues", list.join(", "))]);
        crate::hagency::room_send::send_routed_with(room, RoomMessageEventContent::notice_plain(text), Default::default(), filed_txn_id(&id)).await?;
    }
    match failure {
        Some(error) => Err(crate::i18n::format("Filed {count} of {total}; then GitHub refused: {error}", &[("count", numbers.len().to_string()), ("total", plan.issues.len().to_string()), ("error", error)])),
        None => Ok(numbers),
    }
}

fn filed_txn_id(plan: &str) -> OwnedTransactionId {
    OwnedTransactionId::from(format!("hagency-plan-filed-{}", blake3::hash(plan.as_bytes()).to_hex()))
}

#[cfg(test)]
mod tests {
    use super::owned_agent;
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
}
