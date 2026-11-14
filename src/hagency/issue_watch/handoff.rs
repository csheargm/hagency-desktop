//! The hand-off step: post one request per picked issue and record it as
//! handed off only after the post succeeded. The sender is a trait so the
//! step is tested without Matrix or the ledger.
use ::hagency::issue_watch::Issue;

/// Posts the request and records the issue; the live one is `live::Live`.
pub(crate) trait Handoff {
    /// Post "please fix <url>" for one issue into the project room.
    /// `number` keys the send so a retry of the same issue is not posted twice.
    async fn post(&self, number: i64, text: &str) -> Result<(), String>;
    /// Record that the issue was handed to the Agent.
    async fn mark_seen(&self, number: i64) -> Result<(), String>;
}

/// The words after the Agent mention.
pub(crate) fn request_text(issue: &Issue) -> String {
    format!("please fix {}", issue.url)
}

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
        if let Err(error) = sender.post(issue.number, &request_text(issue)).await {
            outcome.error = Some(crate::i18n::format("Could not hand off issue #{number}: {error}", &[("number", issue.number.to_string()), ("error", error)]));
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
