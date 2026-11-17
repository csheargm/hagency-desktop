//! The actions under an answer delivered inside a run card: the same Copy,
//! Forward, Markdown and PDF actions as an Agent's reply message, applied to
//! the run's answer (never to the request the card hangs under).
use crate::forwarding::ForwardMessage;
use matrix_sdk::ruma::{OwnedEventId, events::room::message::MessageType};

/// Asks the room to forward a run card's answer.
#[derive(Clone, Debug)]
pub struct ForwardRunAnswer {
    pub room: String,
    /// The run's status event, which carries the answer.
    pub run_event: String,
    pub answer: String,
    /// The Agent's Matrix id and name, for an answer whose status event is not loaded.
    pub agent: String,
    pub name: String,
}

/// The room's forwardable messages with the run's status event standing for
/// its answer text, and the event to preselect. A status event no longer in
/// the loaded timeline is added from the card, so Forward always works.
pub fn forward_list(mut messages: Vec<ForwardMessage>, fwd: &ForwardRunAnswer) -> Option<(Vec<ForwardMessage>, OwnedEventId)> {
    let selected: OwnedEventId = fwd.run_event.as_str().try_into().ok()?;
    match messages.iter_mut().find(|m| m.event_id == selected) {
        Some(message) => message.message = MessageType::text_markdown(&fwd.answer),
        None => messages.push(ForwardMessage {
            event_id: selected.clone(), sender: fwd.name.clone(), sender_id: fwd.agent.as_str().try_into().ok()?,
            timestamp: 0, message: MessageType::text_markdown(&fwd.answer),
        }),
    }
    Some((messages, selected))
}

/// Copies or exports the answer here; Forward goes to the room, which holds the history.
#[cfg(feature = "agent_chat")]
pub fn act(cx: &mut makepad_widgets::Cx, action: crate::agent_chat::reply::AnswerAction, entry: &crate::hagency::run_controls::Entry) {
    use crate::agent_chat::reply::AnswerAction;
    let answer = entry.card.answer.as_str();
    match action {
        AnswerAction::Copy => cx.copy_to_clipboard(&crate::shared::mentions_as_names(answer)),
        AnswerAction::Markdown => crate::agent_chat::answer_export::save(answer.to_owned(), false),
        AnswerAction::Pdf => crate::agent_chat::answer_export::save(answer.to_owned(), true),
        AnswerAction::Forward => cx.action(ForwardRunAnswer { room: entry.card.room_id.clone(), run_event: entry.key.clone(),
            answer: answer.to_owned(), agent: entry.agent.clone(), name: entry.name.clone() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::{event_id, user_id};

    fn message(id: &str, body: &str) -> ForwardMessage {
        ForwardMessage { event_id: id.try_into().unwrap(), sender: "a".into(), sender_id: user_id!("@a:x").to_owned(),
            timestamp: 1, message: MessageType::text_plain(body) }
    }
    #[test]
    fn forwarding_a_card_answer_forwards_the_answer_not_the_request() {
        let fwd = ForwardRunAnswer { room: "!r:x".into(), run_event: "$run".into(), answer: "**42**".into(), agent: "@agt:x".into(), name: "Agent".into() };
        let list = vec![message("$request", "question"), message("$run", "run status")];
        let (list, selected) = forward_list(list, &fwd).unwrap();
        assert_eq!(selected, event_id!("$run"));
        assert_eq!(list[1].message.body(), "**42**");
        assert_eq!(list[0].message.body(), "question", "the request is unchanged");
        let (list, _) = forward_list(vec![message("$request", "q")], &fwd).expect("an unloaded status event still forwards");
        assert_eq!((list.len(), list[1].message.body(), list[1].sender_id.as_str()), (2, "**42**", "@agt:x"));
    }
}
