//! What a personal run shows in its room while it works. A person's request
//! gets the run card ("⏳ Working… running …", then the answer) or, without
//! one, the server's processing signal. An automation's request (the issue
//! watcher's hand-off or a review-loop step, `automated_requests`) shows
//! nothing while it runs: the tracker's own lines report its progress and
//! outcome, and its answer is the run's reply (owner, 2026-11-18).

pub(super) struct RunPresence {
    automated: bool,
}

impl RunPresence {
    /// The presence of `requester`'s request `input` to `owner`'s Agent.
    pub(super) fn new(owner: &str, requester: &str, input: &str) -> Self {
        Self { automated: hagency_agent_local::automated_requests::is_automated(owner, requester, input) }
    }
    /// Whether the run posts its card (and its interim text as room
    /// messages); only a host with an approval console can.
    pub(super) fn run_card(&self, has_host: bool) -> bool {
        has_host && !self.automated
    }
    /// Whether the run, once its model starts, signals "processing" in the
    /// room: never twice (a card already says so), never for an automation.
    pub(super) fn processing_signal(&self, carded: bool) -> bool {
        !carded && !self.automated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hagency_runtime::attribution::ISSUE_NOTICE_LEAD;
    const OWNER: &str = "@alice:test";

    #[test]
    fn an_issue_hand_off_runs_with_no_card_and_no_processing_signal() {
        let handoff = format!("{ISSUE_NOTICE_LEAD}#12 in a/b — @agent please fix it on a branch and open a draft pull request");
        let run = RunPresence::new(OWNER, OWNER, &handoff);
        assert!(!run.run_card(true), "no run card for the watcher's hand-off");
        assert!(!run.processing_signal(false), "and no 👀/processing line instead");
    }

    #[test]
    fn a_persons_request_keeps_its_card_exactly_as_before() {
        let typed = RunPresence::new(OWNER, "@bob:test", "@agent please fix #12");
        assert!(typed.run_card(true));
        assert!(!typed.run_card(false), "no approval host: no card, as before");
        assert!(!typed.processing_signal(true), "the card already shows it is working");
        assert!(typed.processing_signal(false), "without a card the processing signal stays");
        let handoff = format!("{ISSUE_NOTICE_LEAD}#12 in a/b");
        assert!(RunPresence::new(OWNER, "@bob:test", &handoff).run_card(true), "a member's look-alike is a person's request");
    }
}
