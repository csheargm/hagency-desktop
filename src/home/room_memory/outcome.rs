//! The visible outcome of an accepted proposal. The card is drawn only by
//! Hagency, so the accepting person's client also posts a short `m.notice`
//! reply to the proposal ("Added to room memory by Anna"): Element and every
//! other client see who decided. The marker key lets Hagency clients reload
//! the room's memory or skills when it arrives.
use matrix_sdk::{Client, ruma::RoomId};
use serde_json::{Value, json};

/// Marks an outcome notice; the value names the proposal and its kind.
pub const OUTCOME_KEY: &str = "io.hagency.proposal_outcome";

/// The proposal message a card was drawn from: its event and thread root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub event_id: String,
    pub thread_root: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Memory,
    Skill,
}
/// What to announce once an Accept was saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announce {
    pub origin: Origin,
    pub kind: Kind,
    pub proposal: String,
}

/// The origin from a raw event: a thread reply stays in its thread.
pub fn origin_from(raw: &Value) -> Option<Origin> {
    let event_id = raw["event_id"].as_str().filter(|id| id.starts_with('$'))?.to_owned();
    let relates = &raw["content"]["m.relates_to"];
    let thread_root = (relates["rel_type"] == "m.thread").then(|| relates["event_id"].as_str().map(String::from)).flatten();
    Some(Origin { event_id, thread_root })
}
pub fn origin_of(event: &matrix_sdk_ui::timeline::EventTimelineItem) -> Option<Origin> {
    let raw = event.latest_json()?;
    origin_from(&serde_json::from_str(raw.json().get()).ok()?)
}

/// The notice text, in the accepting person's language.
pub fn outcome_text(kind: Kind, name: &str) -> String {
    let template = match kind {
        Kind::Memory => "✅ Added to room memory by {name}",
        Kind::Skill => "✅ Saved as a team skill by {name}",
    };
    crate::i18n::format(template, &[("name", name.to_owned())])
}

/// The reply's content: in the proposal's thread when it has one.
pub fn outcome_content(announce: &Announce, name: &str) -> Value {
    let reply = json!({"event_id": announce.origin.event_id});
    let relates = match &announce.origin.thread_root {
        Some(root) => json!({"rel_type": "m.thread", "event_id": root, "is_falling_back": false, "m.in_reply_to": reply}),
        None => json!({"m.in_reply_to": reply}),
    };
    let kind = match announce.kind { Kind::Memory => "memory", Kind::Skill => "skill" };
    json!({"msgtype": "m.notice", "body": outcome_text(announce.kind, name), "m.relates_to": relates,
        OUTCOME_KEY: {"proposal": announce.proposal, "kind": kind}})
}

/// Posts the outcome under the signed-in person's own account.
pub async fn post(client: &Client, room_id: &RoomId, announce: &Announce) -> Result<(), String> {
    let failed = |reason: String| crate::i18n::format("Saved, but could not post the outcome: {reason}", &[("reason", reason)]);
    let room = client.get_room(room_id).ok_or_else(|| failed("room not found".into()))?;
    let me = client.user_id().ok_or_else(|| failed("signed out".into()))?;
    // An unreadable profile falls back to the account's localpart, never an error.
    let name = match room.get_member_no_sync(me).await {
        Ok(Some(member)) => member.display_name().map(str::to_owned),
        _ => None,
    };
    let name = name.unwrap_or_else(|| me.localpart().to_owned());
    room.send_raw("m.room.message", outcome_content(announce, &name)).await.map(|_| ()).map_err(|e| failed(e.to_string()))
}
