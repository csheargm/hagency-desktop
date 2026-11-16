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
}

/// The room's forwardable messages with the run's status event standing for
/// its answer text, and the event to preselect; None when it is not loaded.
pub fn forward_list(mut messages: Vec<ForwardMessage>, run_event: &str, answer: &str) -> Option<(Vec<ForwardMessage>, OwnedEventId)> {
    let message = messages.iter_mut().find(|m| m.event_id.as_str() == run_event)?;
    message.message = MessageType::text_markdown(answer);
    let selected = message.event_id.clone();
    Some((messages, selected))
}

/// Copies or exports the answer here; Forward goes to the room, which holds the history.
#[cfg(feature = "agent_chat")]
pub fn act(cx: &mut makepad_widgets::Cx, action: crate::agent_chat::reply::AnswerAction, room: &str, run_event: &str, answer: &str) {
    use crate::agent_chat::reply::AnswerAction;
    match action {
        AnswerAction::Copy => cx.copy_to_clipboard(answer),
        AnswerAction::Markdown => crate::agent_chat::answer_export::save(answer.to_owned(), false),
        AnswerAction::Pdf => crate::agent_chat::answer_export::save(answer.to_owned(), true),
        AnswerAction::Forward => cx.action(ForwardRunAnswer { room: room.to_owned(), run_event: run_event.to_owned(), answer: answer.to_owned() }),
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
        let list = vec![message("$request", "question"), message("$run", "run status")];
        let (list, selected) = forward_list(list, "$run", "**42**").unwrap();
        assert_eq!(selected, event_id!("$run"));
        assert_eq!(list[1].message.body(), "**42**");
        assert_eq!(list[0].message.body(), "question", "the request is unchanged");
        assert!(forward_list(vec![message("$request", "q")], "$run", "42").is_none());
    }
}
