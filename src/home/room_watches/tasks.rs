//! The room's open Agent tasks (ADR 0023, every room, no GitHub needed):
//! the Agent run cards in the room's recent history, newest revision of each,
//! with who asked, which Agent, working / waiting for approval / done, and
//! age. Read by this app from the timeline it already receives; no Agent runs.
use ::hagency::agent_run_controls::{APPROVAL_WAIT_STEP, Card, State};
use ::hagency::repo_status::AGENT_PUPPET_PREFIX;
use serde_json::Value;
use std::collections::BTreeMap;

/// Timeline events read backwards per load (two pages).
pub const SCAN_EVENTS: u32 = 200;
/// Tasks listed; open ones first.
pub const MAX_TASKS: usize = 12;
/// Finished tasks stay listed this long.
pub const DONE_SECS: i64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Working,
    WaitingForApproval,
    Done,
    Stopped,
}
impl TaskState {
    pub fn words(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::WaitingForApproval => "waiting for approval",
            Self::Done => "done",
            Self::Stopped => "stopped",
        }
    }
    pub fn open(self) -> bool {
        matches!(self, Self::Working | Self::WaitingForApproval)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub agent: String,
    pub requester: String,
    pub state: TaskState,
    /// The live step while working ("running gh pr create").
    pub step: String,
    /// Seconds since the run's card was first posted.
    pub age_secs: i64,
}

fn state_of(card: &Card) -> TaskState {
    match card.state {
        State::Running | State::Stopping if card.progress.as_ref().is_some_and(|p| p.step == APPROVAL_WAIT_STEP) => TaskState::WaitingForApproval,
        State::Running | State::Stopping => TaskState::Working,
        State::Completed => TaskState::Done,
        State::Interrupted | State::Failed | State::Unknown => TaskState::Stopped,
    }
}

/// The card an event carries, whether it is an edit, and the event it belongs to.
fn card_of(event: &Value) -> Option<(String, bool, Card)> {
    let content = &event["content"];
    if content["m.relates_to"]["rel_type"] == "m.replace" {
        let target = content["m.relates_to"]["event_id"].as_str()?;
        return Some((target.to_owned(), true, Card::from_content(&content["m.new_content"])?));
    }
    Some((event["event_id"].as_str()?.to_owned(), false, Card::from_content(content)?))
}

/// Tasks from authentic room-message events (any order). Only a joined
/// Hagency Agent's cards count, and an edit only from its original's sender;
/// an edit whose original is not in the scanned history is dropped.
pub fn tasks(events: &[Value], joined: impl Fn(&str) -> bool, now_ms: i64) -> Vec<Task> {
    let cards: Vec<(String, String, i64, bool, Card)> = events.iter().filter(|e| e["type"] == "m.room.message").filter_map(|event| {
        let sender = event["sender"].as_str().unwrap_or_default();
        if !sender.starts_with(AGENT_PUPPET_PREFIX) || !joined(sender) {
            return None;
        }
        let (target, edit, card) = card_of(event)?;
        Some((target, sender.to_owned(), event["origin_server_ts"].as_i64().unwrap_or(now_ms), edit, card))
    }).collect();
    // (sender, posted at, newest card, its time) by original event id.
    let mut runs: BTreeMap<String, (String, i64, Card, i64)> = BTreeMap::new();
    for (target, sender, ts, _, card) in cards.iter().filter(|c| !c.3) {
        runs.insert(target.clone(), (sender.clone(), *ts, card.clone(), *ts));
    }
    for (target, sender, ts, _, card) in cards.into_iter().filter(|c| c.3) {
        // Same sender and the same run: an edit never swaps in another run's identity.
        if let Some((owner, _, newest, at)) = runs.get_mut(&target)
            && *owner == sender
            && card.revision > newest.revision
            && (card.id.as_str(), card.requester.as_str()) == (newest.id.as_str(), newest.requester.as_str())
        {
            (*newest, *at) = (card, ts);
        }
    }
    // A finished run stays listed for a day from when it finished.
    let mut out: Vec<Task> = runs.into_values().filter(|(_, _, card, at)| state_of(card).open() || (now_ms - at) / 1000 <= DONE_SECS).map(|(agent, first, card, _)| Task {
        agent, requester: card.requester.clone(), state: state_of(&card),
        step: card.progress.as_ref().map(|p| p.step.clone()).filter(|s| s != APPROVAL_WAIT_STEP).unwrap_or_default(),
        age_secs: (now_ms - first).max(0) / 1000,
    }).collect();
    out.sort_by_key(|t| (!t.state.open(), t.age_secs));
    out.truncate(MAX_TASKS);
    out
}

/// The JSON of an authentic event: in an encrypted room only a decrypted
/// event whose encrypting identity is its sender; in a plaintext room any.
pub(crate) fn authentic_json(event: &matrix_sdk::deserialized_responses::TimelineEvent, encrypted: bool) -> Option<Value> {
    use matrix_sdk::deserialized_responses::TimelineEventKind;
    let raw: Value = serde_json::from_str(event.kind.raw().json().get()).ok()?;
    let sender = raw["sender"].as_str().unwrap_or_default();
    let ok = match &event.kind {
        TimelineEventKind::UnableToDecrypt { .. } => false,
        TimelineEventKind::PlainText { .. } => !encrypted,
        TimelineEventKind::Decrypted(d) => !crate::home::room_memory::authentic::identity_untrusted(d.encryption_info.sender.as_str(), &d.encryption_info.verification_state, sender),
    };
    ok.then_some(raw)
}
