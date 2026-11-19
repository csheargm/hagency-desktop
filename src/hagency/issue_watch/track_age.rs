//! Whether a finished record's issue was opened before the work-list entry
//! once it is off the capped open list (GPT batch-3 review: a NeedsHuman
//! record of an old issue was hidden while listed, then published again).
//! The record's own hand-off time answers without a GitHub read when it is
//! older than the entry; otherwise the issue's creation time, which never
//! changes, is remembered for the process (a restart reads it once more).
//! Unknown reads as older: hidden.
use super::tracker::{CLOCK_SLACK_SECS, Inputs, TrackIo, predates};
use ::hagency::issue_watch::Issue;
use ::hagency::issue_watch::issue_track::Track;
use std::collections::HashMap;

type Opened = HashMap<(String, i64), i64>;

fn lock() -> std::sync::MutexGuard<'static, Opened> {
    static OPENED: std::sync::LazyLock<std::sync::Mutex<Opened>> = std::sync::LazyLock::new(Default::default);
    // Plain timestamps: still usable after a panic elsewhere held the lock.
    OPENED.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Remember when each issue (never a pull request) of `repo` was opened.
pub(crate) fn note<'a>(repo: &str, issues: impl IntoIterator<Item = &'a Issue>) {
    let mut opened = lock();
    for issue in issues.into_iter().filter(|i| !i.is_pr) {
        opened.insert((repo.to_ascii_lowercase(), issue.number), issue.created_at);
    }
}

/// For a record missing from the open list: whether its issue was opened
/// before `since`, `None` when neither the record nor memory can tell. A
/// hand-off is never older than its issue, so one before `since` (less the
/// clock slack) proves the issue older.
fn off_list_predates(track: &Track, since: i64) -> Option<bool> {
    if since == i64::MIN {
        return Some(false);
    }
    if track.handed_at < since.saturating_sub(CLOCK_SLACK_SECS) {
        return Some(true);
    }
    lock().get(&(track.repo.to_ascii_lowercase(), track.number)).map(|created| *created < since)
}

/// A finished record missing from the open list (closed, or beyond its page cap).
fn finished_off_list(track: &Track, open_issues: &[Issue]) -> bool {
    track.state.is_terminal() && !open_issues.iter().any(|i| i.number == track.number)
}

/// Whether the room is never shown `track`: its issue predates the entry,
/// listed ([`predates`]) or not (a finished record of unknown age included).
pub(crate) fn hidden(track: &Track, open_issues: &[Issue], since: i64) -> bool {
    predates(open_issues, since, track.number) || (finished_off_list(track, open_issues) && off_list_predates(track, since) != Some(false))
}

/// A finished record whose issue's age only GitHub can tell: the check
/// looks it up ([`hidden_after_lookup`]) before the room's copy is written.
pub(crate) fn unknown(track: &Track, open_issues: &[Issue], since: i64) -> bool {
    finished_off_list(track, open_issues) && off_list_predates(track, since).is_none()
}

/// [`hidden`], with one lookup of an issue of unknown age (remembered); a
/// failed lookup leaves it hidden.
pub(crate) async fn hidden_after_lookup(io: &impl TrackIo, inputs: &Inputs<'_>, track: &Track) -> bool {
    if unknown(track, inputs.open_issues, inputs.since)
        && let Ok((_, Some(issue))) = io.closed_state(track.number).await
    {
        note(inputs.repo, [&issue]);
    }
    hidden(track, inputs.open_issues, inputs.since)
}

#[cfg(test)]
#[path = "track_age_tests.rs"]
mod tests;
