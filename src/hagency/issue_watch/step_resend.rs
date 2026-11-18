//! A `review` or `merge` step whose dispatch never ran is posted again
//! (Nov 18, PR #94: the merge notice reached an Agent whose token budget was
//! used up, and the step, taken once per head, never went out again). The
//! owner API's ledger decides (`stepDue`): it gives the step back when the
//! newest notice this account sent for it ended without the Agent running,
//! never while the budget is used up, after a back-off, and a few times at
//! most. This side posts the round's one room line and re-posts the step
//! as a new message (`again`).
use super::loop_notice::{LoopNotice, Target};
use super::review_loop::LoopIo;
use ::hagency::issue_watch::{DISPATCHED_STEPS, MAX_RESENDS};
use serde_json::{Value, json};

/// Whether a step is taken this check, and the re-send its next post is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Due {
    pub taken: bool,
    pub again: u32,
}

/// The step `step` of `target` this check. A hold step is only looked up;
/// a dispatched step may be given back, and the round's line is said here.
pub(crate) async fn due(io: &impl LoopIo, target: &Target, step: &str) -> Result<Due, String> {
    let mut key = json!({"step": step, "pr": target.url, "head": target.head});
    if !DISPATCHED_STEPS.contains(&step) {
        return Ok(Due { taken: io.review(json!({"stepTaken": key})).await?["taken"] == true, again: 0 });
    }
    key["requester"] = io.requester().into();
    let due = io.review(json!({"stepDue": key})).await?["due"].clone();
    let again = due["again"].as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
    if let Some(notice) = line_of(target, step, &due["say"]) {
        io.post(&notice, 0).await?;
    }
    Ok(Due { taken: due["taken"] == true, again })
}

/// The round's line from the ledger's `say`, keyed by its round.
fn line_of(target: &Target, step: &str, say: &Value) -> Option<LoopNotice> {
    let why = say["why"].as_str()?.to_owned();
    let round = say["round"].as_u64().unwrap_or(0);
    let step_key = format!("not-started-{step}-{round}");
    Some(LoopNotice::NotStarted { target: target.clone(), what: step.to_owned(), why, gave_up: say["gaveUp"] == true, step: step_key })
}

/// The plain room line (English, room content): what could not start, why,
/// and whether it will be tried again.
pub(crate) fn not_started_line(what: &str, why: &str, gave_up: bool, t: &Target) -> String {
    let what = if what == "merge" { "Merge" } else { "Review" };
    let reason = match why {
        "budget" => "the Agent's token budget for this period is used up",
        "noRun" => "an owner limit or a signed-out provider answered before the model started",
        "unrecorded" => "its run was not recorded (sent before this check could see it)",
        _ => "the run was lost or interrupted before it finished",
    };
    let (lead, next) = match (gave_up, why) {
        (true, _) => ("⚠️", format!("it was sent again {MAX_RESENDS} times and will not be retried; check the Agent, then do it yourself")),
        (false, "budget") => ("⏸️", "it will retry when the budget allows".to_owned()),
        (false, _) => ("🔁", "it will retry".to_owned()),
    };
    format!("{lead} {what} of PR #{} in {} could not start — {reason}; {next}. {}", t.number, t.repo, t.url)
}
