//! The daily digest of one issue watch (ADR 0028), pure: which day's window
//! is due, and what the tracker ledger says about it — "Today: N fixed,
//! N merged, N need you", then one line per moved or waiting issue, with a
//! link for each that needs the owner. Each moment belongs to exactly one
//! day's window (yesterday's send hour up to today's), so nothing is counted
//! twice or missed while the app runs every day. A day without moves says
//! nothing. Sending and the once-a-day record are in `digest_live`.
use ::hagency::issue_watch::issue_track::{State, Track};
use ::hagency::issue_watch::{IssueWatch, OwnerRules, WatchMode};
use chrono::{DateTime, Days, NaiveDate, TimeZone};
use serde_json::Value;

/// The local hour (24 h) after which the day's digest is posted.
pub const SEND_HOUR: u32 = 18;
/// Issue lines shown before "And N more".
pub const MAX_ITEMS: usize = 8;

pub const HEADLINE: &str = "📋 {repo} daily digest — Today: {fixed} fixed, {merged} merged, {need} need you";
const MORE: &str = "And {count} more";

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
    pub items: Vec<String>,
}

fn in_window(track: &Track, window: &Window) -> bool {
    (window.start..window.end).contains(&track.changed_at)
}
/// A pull request whose hold (or "needs your review") notice was posted and that has not moved since.
fn held(track: &Track) -> bool {
    track.state == State::Reviewed && track.notified && track.pr.is_some()
}
fn issue_url(repo: &str, number: i64) -> String {
    format!("https://github.com/{repo}/issues/{number}")
}
fn pull_url(repo: &str, pr: u64) -> String {
    format!("https://github.com/{repo}/pull/{pr}")
}

/// `repo`'s digest for `window`, from the ledger's records and the issues
/// whose approval card went unanswered in it; `None` when nothing moved.
pub fn summarize(repo: &str, tracks: &[Track], unanswered: &[i64], window: &Window) -> Option<Digest> {
    let ours: Vec<&Track> = tracks.iter().filter(|t| t.repo.eq_ignore_ascii_case(repo)).collect();
    let moved: Vec<&Track> = ours.iter().copied().filter(|t| in_window(t, window)).collect();
    // Counted once: an issue already held or failed is not also an unanswered card.
    let cards: Vec<i64> = unanswered.iter().copied().filter(|n| !ours.iter().any(|t| t.number == *n && (held(t) || t.state.is_terminal()))).collect();
    if moved.is_empty() && cards.is_empty() {
        return None;
    }
    let fixed: Vec<&Track> = moved.iter().copied().filter(|t| t.state == State::Merged).collect();
    let mut prs: Vec<u64> = fixed.iter().filter_map(|t| t.pr).collect();
    prs.sort_unstable();
    prs.dedup();
    let failed = moved.iter().filter(|t| matches!(t.state, State::Failed | State::NeedsHuman)).count();
    let waiting = ours.iter().filter(|t| held(t)).count();
    let mut shown: Vec<&Track> = ours.iter().copied().filter(|t| in_window(t, window) || held(t)).collect();
    shown.sort_by_key(|t| t.number);
    let mut items: Vec<String> = shown.iter().map(|t| item(repo, t)).collect();
    items.extend(cards.iter().map(|n| crate::i18n::format("#{number} approval card unanswered: {url}", &[("number", n.to_string()), ("url", issue_url(repo, *n))])));
    Some(Digest { fixed: fixed.len(), merged: prs.len(), need: waiting + failed + cards.len(), items })
}

/// One issue's line: "#33 → PR #41 merged", with the link a person needs.
fn item(repo: &str, track: &Track) -> String {
    let number = ("number", track.number.to_string());
    let issue = ("url", issue_url(repo, track.number));
    let Some(pr) = track.pr else {
        return match track.state {
            State::Failed => crate::i18n::format("#{number} gave up: {url}", &[number, issue]),
            State::NeedsHuman => crate::i18n::format("#{number} needs a person: {url}", &[number, issue]),
            State::Closed | State::Merged => crate::i18n::format("#{number} closed", &[number]),
            _ => crate::i18n::format("#{number} handed off", &[number]),
        };
    };
    let args = [number, ("pr", pr.to_string()), ("url", pull_url(repo, pr))];
    let template = match track.state {
        _ if held(track) => "#{number} → PR #{pr} needs you: {url}",
        State::Merged => "#{number} → PR #{pr} merged",
        State::Closed => "#{number} → PR #{pr} closed",
        State::Failed | State::NeedsHuman => "#{number} → PR #{pr} needs a person: {url}",
        _ => "#{number} → PR #{pr} in review",
    };
    crate::i18n::format(template, &args)
}

/// The notice: the headline, then at most [`MAX_ITEMS`] lines.
pub fn text(repo: &str, digest: &Digest) -> String {
    let counts = [("repo", repo.to_owned()), ("fixed", digest.fixed.to_string()), ("merged", digest.merged.to_string()), ("need", digest.need.to_string())];
    let mut lines = vec![crate::i18n::format(HEADLINE, &counts)];
    lines.extend(digest.items.iter().take(MAX_ITEMS).map(|line| format!("• {line}")));
    if digest.items.len() > MAX_ITEMS {
        lines.push(crate::i18n::format(MORE, &[("count", (digest.items.len() - MAX_ITEMS).to_string())]));
    }
    lines.join("\n")
}

// TODO(ADR 0028): a digest-specific ledger query: the Activity view holds the newest
// 20 requests only, and a card counts by its request's receipt, not when it went unanswered.
/// The issues of `repo` whose hand-off run stopped at an approval card
/// nobody answered within `window` (the Activity view's `requests`, already
/// narrowed to this account's own hand-offs).
pub fn unanswered_issues(requests: &Value, repo: &str, window: &Window) -> Vec<i64> {
    let mut numbers: Vec<i64> = requests.as_array().into_iter().flatten()
        .filter(|r| r["unanswered"] == true && r["issue"]["repo"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(repo)))
        .filter(|r| r["receivedAt"].as_i64().is_some_and(|at| (window.start..window.end).contains(&at)))
        .filter_map(|r| r["issue"]["number"].as_i64())
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
