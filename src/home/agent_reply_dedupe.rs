//! A server without the card-reply record posts the Agent's answer again as
//! its own reply, after the run card already carries it. The desktop hides
//! that second copy. It is matched by run, never by text: the copy is the
//! last plain message the run's Agent sent after that run's card and before
//! the Agent's next card in the same thread.
use std::collections::HashSet;
use crate::hagency::run_controls::Entry;

/// One message of a timeline, as the matching needs it.
#[derive(Clone, Copy, Debug)]
pub struct Sent<'a> {
    pub event_id: &'a str,
    pub sender: &'a str,
    /// A plain text or notice message (not a card or other Agent event).
    pub plain: bool,
}

/// A finished run whose card holds its answer.
fn answered(entry: &Entry) -> bool {
    !entry.card.answer.is_empty() && !entry.card.state.active()
}

/// The event ids of the separate replies that repeat an answered card.
pub fn duplicates<'a>(runs: &[Entry], timeline: &[Sent<'a>]) -> HashSet<&'a str> {
    let mut hidden = HashSet::new();
    for run in runs.iter().filter(|run| answered(run)) {
        let Some(start) = timeline.iter().position(|m| m.event_id == run.key) else { continue };
        let own_card = |m: &Sent| m.sender == run.agent && runs.iter().any(|r| r.key == m.event_id);
        let end = timeline[start + 1..].iter().position(own_card).map_or(timeline.len(), |i| start + 1 + i);
        if let Some(copy) = timeline[start + 1..end].iter().rev().find(|m| m.plain && m.sender == run.agent) {
            hidden.insert(copy.event_id);
        }
    }
    hidden
}

/// The reply count a thread bar shows: run cards are drawn on the request,
/// not as replies, and the latest reply is not counted when it repeats an
/// answered card. Zero hides the bar.
pub fn shown_reply_count(count: u32, runs: &[Entry], latest_sender: Option<&str>, latest_plain: bool) -> u32 {
    let cards = runs.len() as u32;
    let copy = latest_plain && latest_sender.is_some_and(|s| runs.iter().any(|r| answered(r) && r.agent == s));
    count.saturating_sub(cards + u32::from(copy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hagency::run_controls::fixture;
    use ::hagency::agent_run_controls::State;

    fn run(key: &str, agent: &str, answer: &str) -> Entry {
        let mut e = fixture(key, if answer.is_empty() { State::Running } else { State::Completed }, answer);
        e.agent = agent.into();
        e
    }
    fn sent<'a>(event_id: &'a str, sender: &'a str, plain: bool) -> Sent<'a> { Sent { event_id, sender, plain } }

    /// Live finding: the server's own reply showed again under the card.
    #[test]
    fn the_separate_reply_of_an_answered_run_is_hidden_matched_by_run() {
        let runs = [run("$card1", "@bot:s", "42"), run("$card2", "@bot:s", "43")];
        let timeline = [
            sent("$card1", "@bot:s", false), sent("$note", "@bot:s", true), sent("$copy1", "@bot:s", true),
            sent("$ask", "@me:s", true),
            sent("$card2", "@bot:s", false), sent("$copy2", "@bot:s", true), sent("$other", "@else:s", true),
        ];
        let hidden = duplicates(&runs, &timeline);
        assert_eq!(hidden, HashSet::from(["$copy1", "$copy2"]));
    }
    #[test]
    fn nothing_is_hidden_for_a_running_run_or_another_senders_message() {
        let runs = [run("$card", "@bot:s", "")];
        let timeline = [sent("$card", "@bot:s", false), sent("$msg", "@bot:s", true)];
        assert!(duplicates(&runs, &timeline).is_empty());
        let runs = [run("$card", "@bot:s", "42")];
        let timeline = [sent("$card", "@bot:s", false), sent("$msg", "@me:s", true)];
        assert!(duplicates(&runs, &timeline).is_empty(), "the requester's follow-up stays");
        assert!(duplicates(&runs, &[sent("$msg", "@bot:s", true)]).is_empty(), "no card loaded, nothing hidden");
    }
    #[test]
    fn the_thread_bar_counts_neither_the_card_nor_its_copy() {
        let runs = [run("$card", "@bot:s", "42")];
        assert_eq!(shown_reply_count(2, &runs, Some("@bot:s"), true), 0, "card + copy: no bar");
        assert_eq!(shown_reply_count(1, &runs, Some("@bot:s"), false), 0, "card only: no bar");
        assert_eq!(shown_reply_count(3, &runs, Some("@me:s"), true), 2, "real replies still count");
        assert_eq!(shown_reply_count(1, &[], Some("@bot:s"), true), 1);
    }
}
