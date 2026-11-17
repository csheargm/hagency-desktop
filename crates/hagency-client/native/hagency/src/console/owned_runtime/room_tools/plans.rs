//! The planner's `plan_propose` for one admitted dispatch (ADR 0017): posts
//! the plan as a card in the dispatch's room and files nothing. The card id
//! is fixed by the dispatch and the plan, so a replay posts the same card.
use super::Source;
use hagency_runtime::room_plan::{self as plans, Plan};
use serde_json::json;
use sha2::{Digest, Sha256};

pub(super) async fn handle(source: &Source, plan: &Plan) -> String {
    let (dispatch, event, _) = super::memory::MemoryIo::identity(source);
    let requester = source.gate.expected.requester_mxid.clone();
    let text = serde_json::to_string(plan).unwrap_or_default();
    let digest = format!("{:x}", Sha256::digest(format!("{dispatch}\0{text}")));
    let id = format!("plan_{}", &digest[..16]);
    let content = plans::card_content(&id, plan, &requester, event);
    match super::memory::MemoryIo::post(source, &format!("hagency_plan_{}", &digest[..24]), content).await {
        Ok(event_id) => json!({"status": "proposed", "plan_id": id, "event_id": event_id,
            "note": "Posted as a card. Nothing is filed until your owner clicks File these issues."}).to_string(),
        Err(_) => json!({"error": "plan_propose_failed"}).to_string(),
    }
}
