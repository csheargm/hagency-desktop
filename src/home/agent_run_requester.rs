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
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use matrix_sdk_ui::timeline::EventTimelineItem;
use std::time::{Duration, Instant};

/// How long after the request the waiting line shows when no card arrives
/// (the owner's app may be closed; the request then simply waits for it).
const PENDING_WINDOW: Duration = Duration::from_secs(10 * 60);

/// A request this viewer sent to an Agent that has not answered yet: the
/// Agent's name and when the request was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRequest {
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
    let age = age(now_ms(), u64::from(event.timestamp().0))?;
    let name = display_name(cx, agent, room);
    Some(PendingRequest { name, sent: Instant::now().checked_sub(age).unwrap_or_else(Instant::now) })
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
mod tests {
    use super::*;
    use crate::i18n::{Language, text_for};

    #[test]
    fn only_an_agent_mention_within_the_window_waits() {
        let people: Vec<OwnedUserId> = ["@anna:x", "@_hagency_agt_01m7k84kgg5a5mtm2epqhbpjb5:y"]
            .iter().map(|u| OwnedUserId::try_from(*u).unwrap()).collect();
        assert_eq!(first_agent(people.iter()).map(|u| u.as_str()), Some("@_hagency_agt_01m7k84kgg5a5mtm2epqhbpjb5:y"));
        assert_eq!(first_agent(people[..1].iter()), None, "a message to a person waits for nobody");
        assert_eq!(age(10_000, 8_000), Some(Duration::from_secs(2)));
        assert_eq!(age(10_000, 12_000), Some(Duration::ZERO), "a clock ahead of ours is no age");
        assert_eq!(age(PENDING_WINDOW.as_millis() as u64 + 1, 0), None, "an old request no longer waits");
    }

    #[test]
    fn the_waiting_line_names_the_agent_and_ticks() {
        let text = pending_text("bird-claude", 12);
        assert!(text.starts_with("⏳") && text.contains("bird-claude") && text.contains(&super::super::agent_run_status::format_elapsed(12)), "{text}");
        let key = "Waiting for {agent} to start… ({t})";
        assert_eq!(text_for(Language::English, key), key);
        let zh = text_for(Language::Chinese, key);
        assert!(zh != key && zh.contains("{agent}") && zh.contains("{t}"), "{zh}");
    }
}
