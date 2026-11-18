//! Where an Agent's answer goes in a group room (ADR 0024).
//!
//! A room chooses with one state event, [`STATE_EVENT`] (`state_key` ""),
//! whose content is `{"mode":"room"}` or `{"mode":"thread"}`. With no event,
//! or one this version does not understand, the room gets [`Mode::Room`]:
//! the answer is an ordinary reply in the main timeline that quotes the
//! request, so every member sees it, as in a chat app.
//!
//! [`Mode::Thread`] restores the earlier behaviour: an answer to a top-level
//! request opens a Matrix thread under it. In either mode a request that a
//! person wrote inside a thread is answered in that thread, and a top-level
//! owner DM (thread root = the room id) stays flat with no quote.
//!
//! The server (`hagency-server` agent-service) carries the same contract for
//! the replies it posts; its strings are pinned by tests there.
use serde_json::{Value, json};

/// The room state event that holds the choice.
pub const STATE_EVENT: &str = "im.hagency.agent_replies";
/// The content field that holds [`Mode::as_str`].
pub const MODE_FIELD: &str = "mode";
/// Longest quoted request in a reply's plain-text fallback, in characters.
pub const QUOTE_CHARS: usize = 80;

/// Where a room's Agents answer top-level requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// In the main timeline, as a reply quoting the request.
    #[default]
    Room,
    /// In a thread under the request.
    Thread,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Room => "room",
            Self::Thread => "thread",
        }
    }
    /// The mode a [`STATE_EVENT`]'s content names; anything else is the default.
    pub fn from_content(content: &Value) -> Self {
        match content.get(MODE_FIELD).and_then(Value::as_str) {
            Some("thread") => Self::Thread,
            _ => Self::Room,
        }
    }
    /// The [`STATE_EVENT`] content that selects this mode.
    pub fn content(self) -> Value {
        json!({ MODE_FIELD: self.as_str() })
    }
}

/// How one Agent message relates to the request it answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement<'a> {
    /// No relation: a top-level owner DM.
    Top,
    /// A main-timeline reply to the request.
    Reply { to: &'a str },
    /// Inside the thread rooted at `root`.
    Thread { root: &'a str },
}

/// Where an Agent message for request `event` goes. `root` is the request's
/// conversation root as the dispatch carries it: the room id for a top-level
/// owner DM, the request itself for a top-level group request, or the root of
/// the thread a person wrote the request in.
pub fn placement<'a>(mode: Mode, room: &str, event: &'a str, root: &'a str) -> Placement<'a> {
    if root == room || !root.starts_with('$') {
        Placement::Top
    } else if root != event || mode == Mode::Thread {
        Placement::Thread { root }
    } else {
        Placement::Reply { to: event }
    }
}

/// The `m.relates_to` for `placement`. A threaded message quotes `target`
/// (or the root) for clients without thread rendering.
pub fn relation(placement: Placement<'_>, target: Option<&str>) -> Option<Value> {
    match placement {
        Placement::Top => None,
        Placement::Reply { to } => Some(json!({"m.in_reply_to": {"event_id": to}})),
        Placement::Thread { root } => Some(json!({
            "rel_type": "m.thread",
            "event_id": root,
            "is_falling_back": true,
            "m.in_reply_to": {"event_id": target.unwrap_or(root)},
        })),
    }
}

/// `body` with the classic plain-text reply fallback in front of it:
/// `> <@sender> first line of the request` and a blank line. Clients that
/// render `m.in_reply_to` strip these lines; plain-text clients show the
/// quote. Only the first line, cut at [`QUOTE_CHARS`] characters; a request
/// with no text leaves `body` as it is.
pub fn with_quote_fallback(sender: &str, request: &str, body: &str) -> String {
    let Some(line) = request.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return body.to_owned();
    };
    let mut quoted: String = line.chars().take(QUOTE_CHARS).collect();
    if line.chars().count() > QUOTE_CHARS {
        quoted.push('…');
    }
    format!("> <{sender}> {quoted}\n\n{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_room_without_the_event_answers_in_the_main_timeline() {
        assert_eq!(Mode::default(), Mode::Room);
        assert_eq!(Mode::from_content(&json!({})), Mode::Room);
        assert_eq!(Mode::from_content(&json!({"mode": "later"})), Mode::Room);
        assert_eq!(Mode::from_content(&json!({"mode": "thread"})), Mode::Thread);
        assert_eq!(Mode::from_content(&Mode::Thread.content()), Mode::Thread);
        assert_eq!(Mode::from_content(&Mode::Room.content()), Mode::Room);
        assert_eq!(STATE_EVENT, "im.hagency.agent_replies", "the server pins the same string");
    }

    #[test]
    fn a_top_level_request_is_answered_where_the_room_chose() {
        let flat = placement(Mode::Room, "!r:x", "$q", "$q");
        assert_eq!(flat, Placement::Reply { to: "$q" });
        assert_eq!(relation(flat, None), Some(json!({"m.in_reply_to": {"event_id": "$q"}})));
        let threaded = placement(Mode::Thread, "!r:x", "$q", "$q");
        assert_eq!(threaded, Placement::Thread { root: "$q" });
        assert_eq!(relation(threaded, Some("$q")).unwrap()["rel_type"], "m.thread");
    }

    #[test]
    fn a_person_started_thread_and_an_owner_dm_keep_their_shape_in_both_modes() {
        for mode in [Mode::Room, Mode::Thread] {
            let inside = placement(mode, "!r:x", "$followup", "$root");
            assert_eq!(inside, Placement::Thread { root: "$root" });
            let value = relation(inside, Some("$followup")).unwrap();
            assert_eq!(value["event_id"], "$root");
            assert_eq!(value["m.in_reply_to"]["event_id"], "$followup");
            assert_eq!(placement(mode, "!dm:x", "$q", "!dm:x"), Placement::Top);
            assert_eq!(relation(Placement::Top, Some("$q")), None);
        }
    }

    #[test]
    fn the_fallback_quotes_one_short_line() {
        let body = with_quote_fallback("@anna:x", "\n  what are the issues?\nsecond", "Three.");
        assert_eq!(body, "> <@anna:x> what are the issues?\n\nThree.");
        let long = "a".repeat(QUOTE_CHARS + 5);
        let quoted = with_quote_fallback("@anna:x", &long, "ok");
        assert!(quoted.starts_with(&format!("> <@anna:x> {}…\n\n", "a".repeat(QUOTE_CHARS))));
        assert_eq!(with_quote_fallback("@a:x", " \n ", "ok"), "ok");
    }
}
