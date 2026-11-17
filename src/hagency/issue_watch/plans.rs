//! The planner's last step on the poller (ADR 0017): for each filed plan of
//! this device's Agent, once every issue is closed as completed, post
//! "Done: #A, #B merged" in the plan's room and mark the plan done. The post
//! uses a transaction id fixed by the plan, so a retry is a duplicate the
//! homeserver drops.
use crate::hagency::backend::Service;
use ::hagency::issue_watch::{Client, GITHUB_API, fetch_issue_done};
use ::hagency::plans::PlanRecord;
use matrix_sdk::ruma::{OwnedRoomId, OwnedTransactionId, events::room::message::RoomMessageEventContent};
use serde_json::{Value, json};

/// The plans of an owner-API reply.
pub(crate) fn open_plans(saved: &Value) -> Vec<PlanRecord> {
    serde_json::from_value(saved["plans"].clone()).unwrap_or_default()
}

/// Reports finished plans; one line per plan that failed.
pub(crate) async fn report_done(service: &Service, client: &Client, agent_id: &str, token: &str, epoch: u64, saved: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    for plan in open_plans(saved) {
        if let Err(error) = finish(service, client, agent_id, token, epoch, &plan).await {
            errors.push(format!("{}: {error}", plan.repo));
        }
    }
    errors
}

async fn finish(service: &Service, client: &Client, agent_id: &str, token: &str, epoch: u64, plan: &PlanRecord) -> Result<(), String> {
    for number in &plan.issues {
        if !fetch_issue_done(client, GITHUB_API, &plan.repo, *number, token).await.map_err(|e| e.to_string())? {
            return Ok(()); // still open: checked again next pass
        }
    }
    super::still_current(epoch)?;
    if crate::hagency::kill_switch::paused_here(agent_id) {
        return Ok(()); // Stop All: nothing is posted until resumed
    }
    let room = OwnedRoomId::try_from(plan.room_id.as_str()).map_err(|_| format!("Invalid room {}", plan.room_id))?;
    let text = ::hagency::room_plan::done_text(&plan.issues);
    let content = RoomMessageEventContent::notice_plain(format!("{text} ({})", plan.repo));
    crate::hagency::room_send::send_routed_with(room, content, Default::default(), done_txn_id(agent_id, &plan.id)).await?;
    super::live::saved(service, agent_id, Some(json!({"plan": {"done": plan.id}}))).await.map(|_| ())
}

pub(crate) fn done_txn_id(agent_id: &str, plan: &str) -> OwnedTransactionId {
    let key = blake3::hash(format!("{agent_id}\n{plan}").as_bytes());
    OwnedTransactionId::from(format!("hagency-plan-done-{}", key.to_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_are_read_from_the_owner_api_reply_and_the_done_post_is_idempotent() {
        let saved = json!({"plans": [{"id": "plan_1", "repo": "a/b", "roomId": "!r:x", "issues": [4, 5]}]});
        assert_eq!(open_plans(&saved)[0].issues, [4, 5]);
        assert!(open_plans(&json!({})).is_empty());
        assert_eq!(done_txn_id("agt", "plan_1"), done_txn_id("agt", "plan_1"));
        assert_ne!(done_txn_id("agt", "plan_1"), done_txn_id("agt", "plan_2"));
    }
}
