//! Where the Agent device puts its answer in a group room (ADR 0024): the
//! room's `im.hagency.agent_replies` choice, read from the device's synced
//! room state, decides between a main-timeline reply that quotes the request
//! and the earlier thread under it.
use super::*;
use hagency_core::agent_replies::{self, Mode, Placement};
use matrix_sdk::ruma::events::{
    StateEventType,
    relation::{InReplyTo, Reply},
    room::message::RoomMessageEventContentWithoutRelation,
};

/// The quoted request is read once; a key that is still missing only drops the quote.
const QUOTE_READ: Duration = Duration::from_secs(5);

impl AgentDevice {
    /// The room's choice. A room without the event, an unreadable one, or a
    /// room this device does not know answers in the main timeline.
    pub(in crate::console) async fn reply_mode(&self, room: &str) -> Mode {
        let Ok(room_id) = OwnedRoomId::try_from(room) else { return Mode::default() };
        let Some(room) = self.client.get_room(&room_id) else { return Mode::default() };
        let event_type = StateEventType::from(agent_replies::STATE_EVENT);
        // A store read error is treated like an absent event: the default is
        // the documented behaviour for a room that never chose (ADR 0024).
        let Ok(Some(raw)) = room.get_state_event(event_type, "").await else { return Mode::default() };
        let value: Option<serde_json::Value> = match raw {
            matrix_sdk::deserialized_responses::RawAnySyncOrStrippedState::Sync(raw) => raw.deserialize_as().ok(),
            matrix_sdk::deserialized_responses::RawAnySyncOrStrippedState::Stripped(raw) => raw.deserialize_as().ok(),
        };
        value.map_or(Mode::default(), |v| Mode::from_content(&v["content"]))
    }

    /// The request's text for the reply's quote: the routed plaintext body,
    /// else the device's decrypted copy. `None` drops the quote, never the reply.
    pub(in crate::console) async fn request_quote(&self, dispatch: &inbox::Dispatch) -> Option<String> {
        if !dispatch.body.is_empty() {
            return Some(dispatch.body.clone());
        }
        let room = self.client.get_room(&OwnedRoomId::try_from(dispatch.room_id.as_str()).ok()?)?;
        let event_id = OwnedEventId::try_from(dispatch.event_id.as_str()).ok()?;
        let event = tokio::time::timeout(QUOTE_READ, room.event(&event_id, None)).await.ok()?.ok()?;
        match &event.kind {
            TimelineEventKind::Decrypted(decrypted) => request_text(decrypted, &dispatch.requester_mxid),
            _ => None,
        }
    }
}

/// [`reply_content`] placed by the room's `mode`. A flat answer replies to the
/// request and carries its quote (`request`) for plain-text clients.
pub(in crate::console) fn placed_reply_content(
    dispatch: &inbox::Dispatch,
    body: &str,
    mode: Mode,
    request: Option<&str>,
) -> Result<RoomMessageEventContent, RuntimeError> {
    let placement = agent_replies::placement(mode, &dispatch.room_id, &dispatch.event_id, &dispatch.thread_root);
    let Placement::Reply { to } = placement else { return reply_content(dispatch, body) };
    let mut content = reply_content(&top_level(dispatch), body)?;
    let event = OwnedEventId::try_from(to).map_err(|_| RuntimeError::Request)?;
    content.relates_to = Some(Relation::Reply(Reply::new(InReplyTo::new(event))));
    if let (Some(request), MessageType::Text(text)) = (request, &mut content.msgtype) {
        text.body = agent_replies::with_quote_fallback(&dispatch.requester_mxid, request, &text.body);
    }
    Ok(content)
}

/// The dispatch as [`reply_content`] sees a top-level answer: no thread.
fn top_level(dispatch: &inbox::Dispatch) -> inbox::Dispatch {
    inbox::Dispatch { thread_root: dispatch.room_id.clone(), ..dispatch.clone() }
}

/// The `m.relates_to` a notice about `dispatch` carries.
pub(in crate::console) fn notice_relation(dispatch: &inbox::Dispatch, mode: Mode) -> Option<Relation<RoomMessageEventContentWithoutRelation>> {
    match agent_replies::placement(mode, &dispatch.room_id, &dispatch.event_id, &dispatch.thread_root) {
        Placement::Top => None,
        Placement::Reply { to } => OwnedEventId::try_from(to)
            .ok()
            .map(|event| Relation::Reply(Reply::new(InReplyTo::new(event)))),
        Placement::Thread { root } => OwnedEventId::try_from(root)
            .ok()
            .map(|root| Relation::Thread(Thread::plain(root.clone(), root))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch(room: &str, thread_root: &str) -> inbox::Dispatch {
        serde_json::from_value(serde_json::json!({"id":"evt_1","bindingId":"bind_1","agentId":"agt_1",
            "eventId":"$request","roomId":room,"requesterMxid":"@anna:example.test","threadRoot":thread_root,
            "body":"","state":"running","bindingGeneration":1})).unwrap()
    }
    fn json(content: &RoomMessageEventContent) -> serde_json::Value {
        serde_json::to_value(content).unwrap()
    }

    /// Betty sees Anna's answer in the main timeline: a reply that quotes
    /// the request, with a plain-text quote for clients without replies.
    #[test]
    fn a_group_request_is_answered_in_the_main_timeline_by_default() {
        let d = dispatch("!team:example.test", "$request");
        let content = json(&placed_reply_content(&d, "**Three** issues.", Mode::Room, Some("what are the issues?")).unwrap());
        assert_eq!(content["m.relates_to"], serde_json::json!({"m.in_reply_to":{"event_id":"$request"}}));
        assert_eq!(content["body"], "> <@anna:example.test> what are the issues?\n\n**Three** issues.");
        assert!(!content["formatted_body"].as_str().unwrap().contains("what are the issues"), "no quote in the rendered HTML");
        let unquoted = json(&placed_reply_content(&d, "Three.", Mode::Room, None).unwrap());
        assert_eq!(unquoted["body"], "Three.");
    }

    #[test]
    fn the_threads_setting_and_person_started_threads_keep_the_thread() {
        let d = dispatch("!team:example.test", "$request");
        let threaded = json(&placed_reply_content(&d, "done", Mode::Thread, Some("q")).unwrap());
        assert_eq!(threaded["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(threaded["body"], "done", "a thread post has no quote");
        let inside = json(&placed_reply_content(&dispatch("!team:example.test", "$root"), "done", Mode::Room, Some("q")).unwrap());
        assert_eq!(inside["m.relates_to"]["event_id"], "$root");
        let dm = json(&placed_reply_content(&dispatch("!dm:example.test", "!dm:example.test"), "done", Mode::Room, Some("q")).unwrap());
        assert!(dm.get("m.relates_to").is_none());
        assert_eq!(dm["body"], "done");
    }

    #[test]
    fn notices_follow_the_same_placement() {
        let d = dispatch("!team:example.test", "$request");
        assert!(matches!(notice_relation(&d, Mode::Room), Some(Relation::Reply { .. })));
        assert!(matches!(notice_relation(&d, Mode::Thread), Some(Relation::Thread(_))));
        assert!(notice_relation(&dispatch("!dm:example.test", "!dm:example.test"), Mode::Room).is_none());
    }
}
