//! The daily digest of one issue watch (ADR 0028), pure: which day's window
//! is due, and what the tracker ledger says about it — "Today: N
//! merged, N need you, N in progress", then one line per moved or waiting issue, with a
//! link for each that needs the owner. Each moment belongs to exactly one
//! day's window (yesterday's send hour up to today's), so nothing is counted
//! twice or missed while the app runs every day. A day without moves says
//! nothing. Sending and the once-a-day record are in `digest_live`; the
//! gate's current holds are in `digest_gate`.
use ::hagency::issue_watch::issue_track::digest::{TrackStep, UnansweredCard};
use ::hagency::issue_watch::issue_track::{MAX_ATTEMPTS, State, Track};
use ::hagency::issue_watch::{IssueWatch, OwnerRules, WatchMode};
use chrono::{DateTime, Days, NaiveDate, TimeZone};
use serde_json::Value;

/// The local hour (24 h) after which the day's digest is posted.
pub const SEND_HOUR: u32 = 18;
/// Issue lines shown before "And N more".
pub const MAX_ITEMS: usize = 8;

/// The headline before its counts, which are joined with ", ".
pub const HEADLINE: &str = "📋 {repo} daily digest — Today: {counts}";
const MERGED: &str = "{count} merged";
/// Only when it differs from the merged count (one PR can fix several issues).
const FIXED: &str = "{count} issues fixed";
const NEED: &str = "{count} need you";
const PROGRESS: &str = "{count} in progress";
const MORE: &str = "And {count} more on {url}";

/// One day's digest window, in Unix seconds: `[start, end)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The local day, `YYYY-MM-DD`: the once-a-day record's key.
    pub day: String,
    pub start: i64,
    pub end: i64,
}

/// The send hour of `date` in `now`'s time zone (the earlier one across a clock change).
fn send_time<Tz: TimeZone>(now: &DateTime<Tz>, date: NaiveDate) -> Option<i64> {
    let local = date.and_hms_opt(SEND_HOUR, 0, 0)?;
    now.timezone().from_local_datetime(&local).earliest().map(|t| t.timestamp())
}

/// Today's window once its send hour has passed, so a start after that hour
/// still sends it; `None` before.
pub fn window_at<Tz: TimeZone>(now: &DateTime<Tz>) -> Option<Window> {
    let today = now.date_naive();
    let end = send_time(now, today)?;
    if now.timestamp() < end {
        return None;
    }
    let start = send_time(now, today.checked_sub_days(Days::new(1))?)?;
    Some(Window { day: today.format("%Y-%m-%d").to_string(), start, end })
}

/// The day's numbers and lines.
// TODO(ADR 0028): show `summarize` for the room's repositories under `/status` (Status tab UI).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Digest {
    /// Issues closed by a merged pull request of the Agent's.
    pub fixed: usize,
    /// Distinct pull requests merged (one can fix several issues).
    pub merged: usize,
    /// Held pull requests, failed issues and unanswered approval cards.
    pub need: usize,
    /// Moved today, still open and waiting on nobody: the Agent is on them.
    pub progress: usize,
    pub items: Vec<String>,
}

fn in_window(at: i64, window: &Window) -> bool {
    (window.start..window.end).contains(&at)
}
fn issue_url(repo: &str, number: i64) -> String {
    format!("https://github.com/{repo}/issues/{number}")
}
fn pull_url(repo: &str, pr: u64) -> String {
    format!("https://github.com/{repo}/pull/{pr}")
}

/// What one repository's digest reads besides the window.
#[derive(Clone, Copy, Debug, Default)]
pub struct Inputs<'a> {
    /// The ledger's records, as they are now.
    pub tracks: &'a [Track],
    /// Every move of those records (the ledger's digest log), oldest first.
    pub steps: &'a [TrackStep],
    /// Issues whose pull request the merge gate holds for the owner right
    /// now (`digest_gate`): not the record's sticky notice flag, which stays
    /// set while a held pull request goes on to merge (integration GPT review).
    pub needs_owner: &'a [i64],
    /// Issues whose approval card went unanswered within the window.
    pub unanswered: &'a [i64],
}

/// `track` as it stood at the window's end, when it moved within it: its
/// last step before the end, so a later move (merged at 18:02 before an
/// 18:05 digest) never hides one before the cutoff (integration GPT review).
/// A record the log has no step of (moved before the log was kept) is
/// judged by its own last move.
fn as_of(track: &Track, steps: &[TrackStep], window: &Window) -> Option<Track> {
    let mine: Vec<&TrackStep> = steps.iter().filter(|s| s.number == track.number && s.repo == track.repo).collect();
    if mine.is_empty() {
        return in_window(track.changed_at, window).then(|| track.clone());
    }
    if !mine.iter().any(|s| in_window(s.at, window)) {
        return None;
    }
    let last = mine.iter().rev().find(|s| s.at < window.end)?;
    Some(Track { state: last.state, pr: last.pr, notified: last.notified, changed_at: last.at, ..track.clone() })
}

/// `repo`'s digest for `window`; `None` when nothing moved.
pub fn summarize(repo: &str, inputs: &Inputs, window: &Window) -> Option<Digest> {
    let ours: Vec<&Track> = inputs.tracks.iter().filter(|t| t.repo.eq_ignore_ascii_case(repo)).collect();
    let held = |t: &Track| !t.state.is_terminal() && t.pr.is_some() && inputs.needs_owner.contains(&t.number);
    let moved: Vec<Track> = ours.iter().filter_map(|t| as_of(t, inputs.steps, window)).collect();
    // Counted once: an issue already held or failed is not also an unanswered card.
    let cards: Vec<i64> = inputs.unanswered.iter().copied().filter(|n| !ours.iter().any(|t| t.number == *n && (held(t) || t.state.is_terminal()))).collect();
    if moved.is_empty() && cards.is_empty() {
        return None;
    }
    let fixed: Vec<&Track> = moved.iter().filter(|t| t.state == State::Merged).collect();
    let mut prs: Vec<u64> = fixed.iter().filter_map(|t| t.pr).collect();
    prs.sort_unstable();
    prs.dedup();
    let failed = moved.iter().filter(|t| matches!(t.state, State::Failed | State::NeedsHuman)).count();
    let waiting: Vec<&Track> = ours.iter().copied().filter(|t| held(t)).collect();
    // A held one shows as it is now; any other moved one as it stood at the cutoff.
    let mut shown: Vec<(i64, String)> = waiting.iter().map(|t| (t.number, item(repo, t, true))).collect();
    shown.extend(moved.iter().filter(|t| !waiting.iter().any(|w| w.number == t.number)).map(|t| (t.number, item(repo, t, false))));
    shown.sort_by_key(|(number, _)| *number);
    let mut items: Vec<String> = shown.into_iter().map(|(_, line)| line).collect();
    items.extend(cards.iter().map(|n| crate::i18n::format("#{number} approval card unanswered: {url}", &[("number", n.to_string()), ("url", issue_url(repo, *n))])));
    // Waiting on the owner (held, or an unanswered card) is "need you", never also in progress.
    let progress = moved.iter().filter(|t| !t.state.is_terminal() && !waiting.iter().any(|w| w.number == t.number) && !cards.contains(&t.number)).count();
    Some(Digest { fixed: fixed.len(), merged: prs.len(), need: waiting.len() + failed + cards.len(), progress, items })
}

/// One issue's line: "#33 → PR #41 merged", with the link a person needs.
fn item(repo: &str, track: &Track, held: bool) -> String {
    let number = ("number", track.number.to_string());
    let issue = ("url", issue_url(repo, track.number));
    let Some(pr) = track.pr else {
        return match track.state {
            State::Failed if tries(track) == 1 => crate::i18n::format("#{number} needs a person: no pull request after 1 try — {url}", &[number, issue]),
            State::Failed => crate::i18n::format("#{number} needs a person: no pull request after {tries} tries — {url}", &[number, ("tries", tries(track).to_string()), issue]),
            State::NeedsHuman => crate::i18n::format("#{number} needs a person: the agent said it cannot fix it — {url}", &[number, issue]),
            State::Closed | State::Merged => crate::i18n::format("#{number} closed", &[number]),
            _ => crate::i18n::format("#{number} handed off", &[number]),
        };
    };
    let args = [number, ("pr", pr.to_string()), ("url", pull_url(repo, pr))];
    let template = match track.state {
        _ if held => "#{number} → PR #{pr} needs you: {url}",
        State::Merged => "#{number} → PR #{pr} merged",
        State::Closed => "#{number} → PR #{pr} closed",
        State::Failed | State::NeedsHuman => "#{number} → PR #{pr} needs a person: {url}",
        _ => "#{number} → PR #{pr} in review",
    };
    crate::i18n::format(template, &args)
}

/// The attempts a given-up record made (the ledger's count, else the cap).
fn tries(track: &Track) -> u32 {
    if track.attempts == 0 { MAX_ATTEMPTS } else { track.attempts }
}

/// "7 merged, 5 need you, 2 in progress": each count says something
/// different; issues fixed shows only when it is not the merged count.
fn counts(digest: &Digest) -> String {
    let count = |template: &str, n: usize| crate::i18n::format(template, &[("count", n.to_string())]);
    let mut parts = vec![count(MERGED, digest.merged)];
    if digest.fixed != digest.merged { parts.push(count(FIXED, digest.fixed)); }
    parts.push(count(NEED, digest.need));
    if digest.progress > 0 { parts.push(count(PROGRESS, digest.progress)); }
    parts.join(crate::i18n::tr(", "))
}

/// The notice: the headline, then at most [`MAX_ITEMS`] lines, then where the rest are.
pub fn text(repo: &str, digest: &Digest) -> String {
    let mut lines = vec![crate::i18n::format(HEADLINE, &[("repo", repo.to_owned()), ("counts", counts(digest))])];
    lines.extend(digest.items.iter().take(MAX_ITEMS).map(|line| format!("• {line}")));
    if digest.items.len() > MAX_ITEMS {
        let url = format!("https://github.com/{repo}/issues");
        lines.push(crate::i18n::format(MORE, &[("count", (digest.items.len() - MAX_ITEMS).to_string()), ("url", url)]));
    }
    lines.join("\n")
}

/// The issues of `repo` whose approval card, on a hand-off `me` sent, went
/// unanswered within `window`: timed by the card, from the ledger's own
/// log, so heavy traffic hides none and a card that expires after the
/// cutoff counts in the next day's digest (integration GPT review).
pub fn unanswered_issues(cards: &[UnansweredCard], me: &str, repo: &str, window: &Window) -> Vec<i64> {
    let mut numbers: Vec<i64> = cards.iter()
        .filter(|c| c.requester == me && c.repo.eq_ignore_ascii_case(repo) && in_window(c.at, window))
        .map(|c| c.number)
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

/// Every watched repository and its report room: each work-list entry, and
/// the single watch while it is on (the same repository once).
pub fn report_rooms(saved: &Value) -> Vec<(String, String)> {
    // Unreadable parts add no room here: the check already reports them as its own errors.
    let work: Vec<OwnerRules> = serde_json::from_value(saved["work"].clone()).unwrap_or_default();
    let watch: Option<IssueWatch> = serde_json::from_value(saved["issueWatch"].clone()).ok().filter(|w: &IssueWatch| w.setting.mode != WatchMode::Off);
    let mut rooms: Vec<(String, String)> = Vec::new();
    let all = work.into_iter().map(|e| (e.repo, e.room_id)).chain(watch.map(|w| (w.setting.repo, w.setting.room_id)));
    for (repo, room) in all {
        if !repo.is_empty() && !room.is_empty() && !rooms.iter().any(|(r, _)| r.eq_ignore_ascii_case(&repo)) {
            rooms.push((repo, room));
        }
    }
    rooms
}

/// Whether `day`'s digest for `repo` was already posted (the ledger's record).
pub fn sent(saved: &Value, repo: &str, day: &str) -> bool {
    saved["digestSent"][repo.to_ascii_lowercase()].as_str().is_some_and(|last| last >= day)
}
