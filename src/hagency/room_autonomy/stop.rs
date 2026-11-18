//! Pause stops the loop's runs in progress in the room (ADR 0030), through
//! the run card's own "Stop this run": only runs this owner may stop whose
//! request is one of the loop's own notices (an issue hand-off, a review or
//! merge step, a tracker follow-up) sent by this account. A plain @mention
//! answer keeps running: Pause stops the loop, not the conversation.
use crate::hagency::issue_watch::{loop_notice, notice};
use serde_json::Value;

/// The markers the loop's own requests carry.
const LOOP_MARKERS: [&str; 3] = [notice::MARKER, loop_notice::MARKER, crate::hagency::issue_watch::TRACK_MARKER];

/// Whether a request event (`type`, `sender`, `content`) is one of the loop's
/// own requests sent by `me`.
pub fn is_loop_request(event: &Value, me: &str) -> bool {
    event["sender"] == me && LOOP_MARKERS.iter().any(|m| event["content"].get(*m).is_some())
}

/// Stop the loop's runs in `room`; how many were asked to stop.
pub async fn stop_loop_runs(room: &matrix_sdk::Room) -> usize {
    let Some(me) = room.client().user_id().map(|u| u.to_string()) else { return 0 };
    let mut stopped = 0;
    for (key, event_id) in crate::hagency::run_controls::stoppable_in(room.room_id().as_str()) {
        let Ok(id) = matrix_sdk::ruma::OwnedEventId::try_from(event_id.as_str()) else { continue };
        // An event that cannot be read is not known to be the loop's: it keeps running.
        let Ok(event) = room.load_or_fetch_event(&id, None).await else { continue };
        let Ok(raw) = serde_json::from_str::<Value>(event.raw().json().get()) else { continue };
        if is_loop_request(&raw, &me) && crate::hagency::run_controls::stop_one(&key) {
            stopped += 1;
        }
    }
    stopped
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_loops_own_requests_stop_never_an_at_mention() {
        let handoff = json!({"sender": "@o:x", "content": {"body": "@bird please fix", notice::MARKER: {"repo": "a/b"}}});
        assert!(is_loop_request(&handoff, "@o:x"));
        let review = json!({"sender": "@o:x", "content": {"body": "review", loop_notice::MARKER: {"step": "review"}}});
        assert!(is_loop_request(&review, "@o:x"));
        let mention = json!({"sender": "@o:x", "content": {"body": "@bird what does this do?"}});
        assert!(!is_loop_request(&mention, "@o:x"), "a plain @mention keeps running");
        let other = json!({"sender": "@anna:x", "content": {"body": "x", notice::MARKER: {}}});
        assert!(!is_loop_request(&other, "@o:x"), "only this account's own loop notices");
    }
}
