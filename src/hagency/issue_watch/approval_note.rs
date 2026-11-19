//! The merge step's approval line in plain words (live PR #121, Nov 18):
//! bird-codex, bird's Agent, approved anna-claude's pull request through
//! bird's own Hagency, which records it on bird's device only, so Anna's
//! gate never counts it. The room was told the verdict "was not posted
//! through the merge gate" (wrong), with the gate's reason quoted and cut
//! mid-word. Now: at Assist, when one more approval would leave only the
//! person, ONE line says who reviewed it and that a person merges it;
//! otherwise the hold names the reviewer and why its review does not count.
//! The gate's decision is never changed: a person still merges.
use super::reviewer_pick::{Peers, Pick};
use ::hagency::issue_watch::safeguards::room_autonomy::ASSIST_REASON;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use serde_json::Value;
use std::time::Duration;

/// The readiness field holding the plain approval line ([`super::merge_hold::gate_of`] reads it).
pub const APPROVAL_LINE_KEY: &str = "approvalLine";
/// The readiness field naming who reviewed a pull request only a person merges now.
pub const REVIEWED_BY_KEY: &str = "reviewedBy";
/// The longest name said for a reviewer or its owner (room display names are anyone's).
const MAX_NAME_CHARS: usize = 48;
/// How long the reviewer's owner is looked up for before the line says "another owner".
const OWNER_TIMEOUT: Duration = Duration::from_secs(5);

/// The reviewer the loop asked, as the room reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reviewer {
    pub name: String,
    /// Another owner's display name; `None` when unknown.
    pub owner: Option<String>,
    /// One of this owner's own Agents.
    pub own: bool,
}

impl Reviewer {
    /// "bird-codex (bird's agent)", or just the name for this owner's own Agent.
    pub fn label(&self) -> String {
        match (self.own, &self.owner) {
            (true, _) => self.name.clone(),
            (false, Some(owner)) => format!("{} ({owner}'s agent)", self.name),
            (false, None) => format!("{} (another owner's agent)", self.name),
        }
    }
}

/// `name` on one line, at most [`MAX_NAME_CHARS`], never cut inside a character.
fn plain_name(name: &str) -> String {
    let words = name.split(|c: char| c.is_whitespace() || c.is_control()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    words.chars().take(MAX_NAME_CHARS).collect()
}

/// What the room shows for the hold line when the gate misses an Agent
/// approval: who was asked, whether its verdict is on GitHub (`verdict`),
/// and why it does not count. `counted`: approvals the gate already counts.
pub fn approval_line(reviewer: Option<&Reviewer>, verdict: bool, counted: u64) -> String {
    let lead = if counted == 0 { "an Agent approval that counts here".to_owned() } else { format!("more Agent approvals ({counted} counted so far)") };
    const ELSEWHERE: &str = "but another owner's Agent's review is recorded only on that owner's device";
    const OWN_AGENTS: &str = "Merge it yourself, or ask one of your own Agents to review it.";
    match reviewer {
        Some(r) if !r.own && verdict => format!("{lead}. {} was asked to review it and an approval of this version is on GitHub, {ELSEWHERE}, so it does not count here. {OWN_AGENTS}", r.label()),
        Some(r) if !r.own => format!("{lead}. {} was asked to review it, {ELSEWHERE}, so it would not count here. {OWN_AGENTS}", r.label()),
        Some(r) if verdict => format!("{lead}. An approval of this version is on GitHub, but none that counts here is recorded, even after {name} was asked again. Merge it yourself, or ask {name} to review it again.", name = r.name),
        Some(r) => format!("{lead}. {} was asked to review it, and no approval from it is recorded for this version yet. Merge it yourself, or wait for the review.", r.name),
        None => format!("{lead}. No further approval of this version is recorded yet. Merge it yourself, or ask an Agent in the room to review it."),
    }
}

/// Whether one more approval would leave only the room's Assist level: the
/// gate's `oneMore` answer, for the same head.
pub(super) fn only_assist_left(readiness: &Value) -> bool {
    let more = &readiness["oneMore"];
    more["decision"] == "human" && more["reason"] == ASSIST_REASON && more["head"] == readiness["head"]
}

/// `readiness` for the hold when an Agent approval is missing: with a
/// verdict of this head on GitHub (`login`'s, [`super::review_again::unrecorded_verdict`])
/// from the asked `reviewer` and only Assist left, the gate's `oneMore`
/// answer (a person merges) naming the reviewer; else the plain approval line.
pub fn for_hold(mut readiness: Value, (reviews, login): (&[(String, String, String)], &str), reviewer: Option<&Reviewer>) -> Value {
    let Some(head) = readiness["head"].as_str().map(str::to_owned) else { return readiness };
    if readiness["kind"] != "approvals" {
        return readiness;
    }
    let verdict = super::review_again::unrecorded_verdict(&readiness, (reviews, login), &head);
    if let Some(r) = reviewer.filter(|_| verdict && only_assist_left(&readiness)) {
        let mut ready = readiness["oneMore"].clone();
        ready[REVIEWED_BY_KEY] = r.label().into();
        return ready;
    }
    readiness[APPROVAL_LINE_KEY] = approval_line(reviewer, verdict, readiness["approvers"].as_u64().unwrap_or(0)).into();
    readiness
}

/// The reviewer asked again for an unrecorded verdict: only this owner's
/// own, never another owner's Agent, whose verdict only its owner's device
/// records, so asking again cannot help (PR #121).
pub(super) fn asked_again(peer: Option<&Pick>) -> Option<&Pick> {
    peer.filter(|p| p.own)
}

/// The asked reviewer `peer` as the room reads it, with its owner's name
/// when it is another owner's Agent (`None` after [`OWNER_TIMEOUT`]).
pub(super) async fn reviewer_of(peers: &Peers, peer: Option<&Pick>, room: &OwnedRoomId) -> Option<Reviewer> {
    let peer = peer?;
    let name = peers.candidates.iter().find(|c| c.puppet == peer.reviewer).map_or_else(|| peer.reviewer.localpart().to_owned(), |c| c.name.clone());
    let owner = if peer.own { None } else { tokio::time::timeout(OWNER_TIMEOUT, owner_name(room, &peer.reviewer)).await.ok().flatten() };
    Some(Reviewer { name: plain_name(&name), owner: owner.map(|o| plain_name(&o)), own: peer.own })
}

/// The room's name for the owner of `agent` (its listing's, else its
/// published profile's), or the owner's user name when not in the room.
async fn owner_name(room: &OwnedRoomId, agent: &OwnedUserId) -> Option<String> {
    let me = crate::sliding_sync::current_user_id()?;
    // Unreadable owners: the line says "another owner's agent", still true.
    let (owners, _) = crate::profile::agent_profile_owner::member_owners(room, &me).await.ok()?;
    let owner = owners.get(agent)?;
    let joined = crate::sliding_sync::get_client().and_then(|c| c.get_room(room));
    let member = match joined {
        Some(joined) => joined.get_member_no_sync(owner).await.ok().flatten(),
        None => None,
    };
    Some(member.and_then(|m| m.display_name().map(str::to_owned)).unwrap_or_else(|| owner.localpart().to_owned()))
}

#[cfg(test)]
#[path = "approval_note_tests.rs"]
mod tests;
