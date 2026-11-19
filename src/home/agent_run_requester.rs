//! What the person who asked an Agent sees while someone else's Agent works.
//!
//! Chris's test (Nov 17): Anna asked Betty's Agent and saw nothing for 35 s.
//! The Agent's run card is a room message, so Anna's app shows it once it
//! exists, but it only exists once the owner's runtime starts the turn (33 s
//! that day: the server stalled; 8 s is usual). Until then this shows, on
//! Anna's own message, "⏳ Waiting for bird-claude to start… (12 s)". Once the
//! card exists it says the Agent works, or "⏳ Waiting for Betty's approval"
//! while the run waits on its owner (`agent_run_footer::waiting`). Only what
//! Anna can see is used: her own message's mentions and the room's run card.
use makepad_widgets::Cx;
use matrix_sdk::ruma::{events::room::message::MessageType, OwnedRoomId, OwnedUserId, UserId};
use matrix_sdk_ui::timeline::EventTimelineItem;
use std::time::{Duration, Instant};

/// How long after the request the waiting line shows when no card arrives
/// (the owner's app may be closed; the request then simply waits for it).
const PENDING_WINDOW: Duration = Duration::from_secs(10 * 60);

/// A request this viewer sent to an Agent that has not answered yet: the
/// Agent, its name and when the request was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRequest {
    pub agent: OwnedUserId,
    pub name: String,
    pub sent: Instant,
}

/// The pending request behind this timeline message, if it is one: the
/// viewer's own message naming an Agent, recent, not failed to send, with no
/// reply in its thread.
/// Callers ask only when the message has no run card yet.
pub(crate) fn pending_request(cx: &mut Cx, event: &EventTimelineItem, room: &OwnedRoomId) -> Option<PendingRequest> {
    use matrix_sdk_ui::timeline::EventSendState;
    if !event.is_own()
        || matches!(event.send_state(), Some(EventSendState::SendingFailed { .. }))
        || event.content().thread_summary().is_some_and(|s| s.num_replies > 0)
    {
        return None;
    }
    let message = event.content().as_message()?;
    let agent = first_agent(message.mentions()?.user_ids.iter())?;
    let sender = event.sender();
    let age = waiting_age(now_ms(), u64::from(event.timestamp().0), (room.as_str(), sender.as_str()), message.body(),
        || crate::home::agent_ask_badge::is_own_agent(sender, agent),
        || crate::home::room_memory::auto::load_roster(room))?;
    let pill = match message.msgtype() {
        MessageType::Text(text) => text.formatted.as_ref().and_then(|f| pill_name(&f.body, agent)),
        _ => None,
    };
    let name = agent_name(cx, agent, room, pill.as_deref());
    Some(PendingRequest { agent: agent.clone(), name, sent: Instant::now().checked_sub(age).unwrap_or_else(Instant::now) })
}

/// An Agent's name in `room` (asked for when not cached yet), else `hint`
/// (its mention on the request, its name on its run card), else the agent
/// registry's, never its raw `_hagency_agt_…` ID: Nov 18, a new reply
/// showed that ID for a second before the member loaded.
pub(crate) fn agent_name(cx: &mut Cx, agent: &UserId, room: &OwnedRoomId, hint: Option<&str>) -> String {
    let cached = crate::profile::user_profile_cache::get_user_display_name_for_room(cx, agent.to_owned(), Some(room), true).into_option();
    pick_agent_name(&[cached.as_deref(), hint], agent)
        .or_else(|| pick_agent_name(&[registry_name(agent).as_deref()], agent))
        .unwrap_or_else(|| crate::i18n::tr("Agent").to_owned())
}

/// The name the agent registry keeps for `agent`, if any.
fn registry_name(agent: &UserId) -> Option<String> {
    crate::agent_access::current().agent_registry.agents()
        .find(|(id, _)| id.as_str() == agent.as_str())
        .and_then(|(_, entry)| entry.display_name.clone())
}

/// The first of `names` that is a real name, not `agent`'s raw ID.
pub(crate) fn pick_agent_name(names: &[Option<&str>], agent: &UserId) -> Option<String> {
    let raw = |name: &str| name == agent.as_str() || name == agent.localpart() || name.starts_with("_hagency_");
    names.iter().flatten()
        .map(|name| name.trim().trim_start_matches('@'))
        .find(|name| !name.is_empty() && !raw(name))
        .map(str::to_owned)
}

/// The text of the mention pill for `agent` in a message's HTML body: an
/// anchor whose `href` is exactly a matrix.to link to `agent`.
fn pill_name(html: &str, agent: &UserId) -> Option<String> {
    html.match_indices("<a ").find_map(|(at, _)| {
        let tag = &html[at..at + html[at..].find('>')?];
        let href = ["href=\"", "href='"].into_iter().find_map(|open| {
            let start = tag.find(open)? + open.len();
            let close = open.chars().last()?;
            Some(&tag[start..start + tag[start..].find(close)?])
        })?;
        let target = href.strip_prefix("https://matrix.to/#/")?;
        let target = target.split('?').next().unwrap_or_default().replace("%40", "@").replace("%3A", ":").replace("%3a", ":");
        if target != agent.as_str() { return None; }
        let body = &html[at + tag.len() + 1..];
        Some(html_text(&body[..body.find("</a>")?]))
    })
}

/// The visible text of an HTML fragment: tags dropped, entities decoded.
fn html_text(fragment: &str) -> String {
    let (mut text, mut in_tag) = (String::new(), false);
    for c in fragment.chars() {
        match c { '<' => in_tag = true, '>' => in_tag = false, c if !in_tag => text.push(c), _ => {} }
    }
    // `&amp;` last, so "&amp;lt;" stays the text "&lt;".
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&").trim().to_owned()
}

/// Whether the viewer's own message is one of its loop's notices ("🔎 Review
/// requested", a hand-off) to one of the viewer's own Agents (`own_agent`):
/// that run posts no card by design (the tracker's lines report it), so
/// nothing waits for one (live PR #109, Nov 18). Another owner's Agent still
/// posts its card, so the line waits for it (GPT batch review).
fn waits_for_no_card(sender: &str, body: &str, own_agent: bool) -> bool {
    own_agent && ::hagency::issue_watch::automated_requests::is_automated(sender, sender, body)
}

/// How long ago a still-waiting request was sent, `None` once its window
/// passed or for the viewer's loop notice to its own Agent. The age is checked
/// first: an expired notice never asks for the Agent listing (GPT P2, Nov 19).
/// Whether the notice is to an own Agent is unknown until the listing is read:
/// the line shows meanwhile and `read_listing` asks for it, since nothing else
/// reads it unless the member list opened (bird's window, Nov 19). The card's
/// clock tick re-populates the row, so the line goes once the listing arrives.
/// A review request whose own reviewer's review the loop recorded in this
/// `room` waits no more (bird's window, Nov 18: "⏳ Waiting for bird-codex to
/// start…" stayed after bird-codex had reviewed).
fn waiting_age(now_ms: u64, sent_ms: u64, (room, sender): (&str, &str), body: &str, own_agent: impl FnOnce() -> Option<bool>, read_listing: impl FnOnce()) -> Option<Duration> {
    let age = age(now_ms, sent_ms)?;
    if crate::hagency::issue_watch::loop_notice::answered(room, body) {
        return None;
    }
    let (hide, read) = own_notice_decision(sender, body, own_agent);
    if read {
        read_listing();
    }
    (!hide).then_some(age)
}

/// (hide the line, read the Agent listing) for a message: only the viewer's
/// loop notices consult `own_agent`, and an unknown answer asks for the listing.
fn own_notice_decision(sender: &str, body: &str, own_agent: impl FnOnce() -> Option<bool>) -> (bool, bool) {
    if !waits_for_no_card(sender, body, true) {
        return (false, false);
    }
    let own = own_agent();
    (own == Some(true), own.is_none())
}

/// The first Agent a message mentions.
fn first_agent<'a>(mut mentioned: impl Iterator<Item = &'a OwnedUserId>) -> Option<&'a OwnedUserId> {
    mentioned.find(|user| ::hagency::agent_questions::is_agent(user.as_str()))
}

/// How long ago a request sent at `sent_ms` was, while it may still wait.
fn age(now_ms: u64, sent_ms: u64) -> Option<Duration> {
    let age = Duration::from_millis(now_ms.saturating_sub(sent_ms));
    (age < PENDING_WINDOW).then_some(age)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// A member's name in this room as this app knows it (asked for when not
/// cached yet); the user ID's local part until then.
pub(crate) fn display_name(cx: &mut Cx, user: &OwnedUserId, room: &OwnedRoomId) -> String {
    crate::profile::user_profile_cache::get_user_display_name_for_room(cx, user.clone(), Some(room), true)
        .into_option()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| user.localpart().to_owned())
}

/// The waiting line under a request no card answers yet.
pub(crate) fn pending_text(name: &str, elapsed_secs: u64) -> String {
    let elapsed = super::agent_run_status::format_elapsed(elapsed_secs);
    format!("⏳ {}", crate::i18n::format("Waiting for {agent} to start… ({t})", &[("agent", name.to_owned()), ("t", elapsed)]))
}

#[cfg(test)]
#[path = "agent_run_requester_tests.rs"]
mod tests;
