//! The hand-off step: post one request per picked issue and record it as
//! handed off only after the post succeeded. The sender is a trait so the
//! step is tested without Matrix or the ledger.
use ::hagency::issue_watch::Issue;

/// Posts the request and records the issue; the live one is `live::Live`.
pub(crate) trait Handoff {
    /// Post the watcher's notice for one issue (`notice::Notice`) into the
    /// project room; the issue number keys the send so a retry is not posted twice.
    async fn post(&self, issue: &Issue) -> Result<(), String>;
    /// Record that the issue was handed to the Agent.
    async fn mark_seen(&self, number: i64) -> Result<(), String>;
    /// The owner's Stop All pauses this Agent now (`kill_switch`).
    fn paused(&self) -> bool;
}

/// Shown when Stop All paused the Agent during a check.
pub const PAUSED: &str = "Agent paused by the owner: issue #{number} waits until it is resumed";

/// What one check did: how many issues went to the Agent, and the first
/// failure, which stops the rest so they are retried next check.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub handed: usize,
    pub error: Option<String>,
}

pub(crate) async fn hand_off(sender: &impl Handoff, picks: &[&Issue]) -> Outcome {
    let mut outcome = Outcome::default();
    for issue in picks {
        // Checked right before each step: a check may run for many seconds,
        // and Stop All must fence a hand-off already in flight. A paused issue
        // is not marked seen, so it is handed off after Resume (a repeat post
        // carries the same transaction id and is deduplicated).
        if sender.paused() {
            outcome.error = Some(paused_line(issue.number));
            break;
        }
        if let Err(error) = sender.post(issue).await {
            outcome.error = Some(crate::i18n::format("Could not hand off issue #{number}: {error}", &[("number", issue.number.to_string()), ("error", error)]));
            break;
        }
        if sender.paused() {
            outcome.error = Some(paused_line(issue.number));
            break;
        }
        // Posted but not recorded: the next check posts it again with the
        // same transaction id (deduplicated), and the owner sees this.
        if let Err(error) = sender.mark_seen(issue.number).await {
            outcome.error = Some(crate::i18n::format("Issue #{number} was handed off but not recorded: {error}", &[("number", issue.number.to_string()), ("error", error)]));
            break;
        }
        outcome.handed += 1;
    }
    outcome
}
fn paused_line(number: i64) -> String {
    crate::i18n::format(PAUSED, &[("number", number.to_string())])
}
