//! The tracker's live I/O (ADR 0018): GitHub reads with the owner's token
//! (never a write besides the existing claim comment), room lines as
//! `m.notice` (never dispatched as a request), hand-offs through `live::Live`,
//! pull request follow-through through the review loop (`review_loop`), and
//! the owner API's `track` record.
use super::review_loop::{self, LoopCtx};
use super::tracker::{self, Followed, Followups, Inputs, TrackIo};
use super::work::{Holder, LiveLoop, Worker};
use ::hagency::issue_watch::issue_track::{IssueFact, Track, says_cannot_fix};
use ::hagency::issue_watch::{GITHUB_API, Issue, IssueWatch, OwnerRules, PullRequest, WatchMode, fetch_comments, fetch_open_issues, fetch_issue_state, fetch_login, fetch_open_pulls, fetch_permission};
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::{OwnedRoomId, OwnedTransactionId, OwnedUserId};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

/// The content field that marks a tracker line.
pub const MARKER: &str = "io.hagency.issue_track";
/// GitHub's and this device's clocks may differ; a cannot-fix comment this
/// much older than the hand-off still counts for it.
const CLOCK_SLACK_SECS: i64 = 120;

/// One watched repository as the check sees it.
pub(crate) struct Watched<'a> {
    pub watch: &'a IssueWatch,
    /// The work-list entry, when the repository comes from the work list (claims apply).
    pub entry: Option<&'a OwnerRules>,
    pub open_issues: &'a [Issue],
    pub seen: &'a HashSet<i64>,
}

/// Follow every tracked issue of one repository; failures become errors in the result.
pub(crate) async fn follow_repo(worker: &Worker<'_>, watched: &Watched<'_>, saved: &Value) -> Followups {
    match follow_inner(worker, watched, saved).await {
        Ok(followups) => followups,
        Err(error) => Followups { lines: Vec::new(), errors: vec![format!("{}: {error}", watched.watch.setting.repo)] },
    }
}

/// A work-list repository: its open issues are read again here, and a
/// repository the single-repository watch also follows is left to that one.
pub(crate) async fn follow_work(worker: &Worker<'_>, entry: &OwnerRules, watch: &IssueWatch, saved: &Value, seen: &HashSet<i64>) -> Followups {
    let legacy: Option<IssueWatch> = serde_json::from_value(saved["issueWatch"].clone()).ok();
    if legacy.is_some_and(|w| w.setting.mode != WatchMode::Off && w.setting.repo.eq_ignore_ascii_case(&entry.repo)) {
        return Followups::default();
    }
    let issues = match fetch_open_issues(worker.client, GITHUB_API, &entry.repo, worker.token).await {
        Ok(issues) => issues,
        Err(error) => return Followups { lines: Vec::new(), errors: vec![format!("{}: {error}", entry.repo)] },
    };
    follow_repo(worker, &Watched { watch, entry: Some(entry), open_issues: &issues, seen }, saved).await
}

async fn follow_inner(worker: &Worker<'_>, watched: &Watched<'_>, saved: &Value) -> Result<Followups, String> {
    let repo = watched.watch.setting.repo.as_str();
    let tracks: Vec<Track> = if saved["track"].is_null() { Vec::new() } else { serde_json::from_value(saved["track"].clone()).map_err(|e| format!("Unreadable issue tracks: {e}"))? };
    let ours = |t: &&Track| t.repo.eq_ignore_ascii_case(repo);
    let tracked: HashSet<i64> = tracks.iter().filter(ours).map(|t| t.number).collect();
    let busy = tracks.iter().filter(ours).any(|t| !t.state.is_terminal())
        || watched.open_issues.iter().any(|i| watched.seen.contains(&i.number) && !tracked.contains(&i.number));
    let now = chrono::Utc::now().timestamp();
    if !busy {
        // Only finished records: their lines need no GitHub read.
        return Ok(Followups { lines: tracks.iter().filter(ours).filter_map(tracker::terminal_line).collect(), errors: Vec::new() });
    }
    let login = fetch_login(worker.client, GITHUB_API, worker.token).await.map_err(|e| e.to_string())?;
    let pulls = fetch_open_pulls(worker.client, GITHUB_API, repo, worker.token).await.map_err(|e| e.to_string())?;
    let own: Vec<PullRequest> = pulls.into_iter().filter(|p| p.author.eq_ignore_ascii_case(&login)).collect();
    let permission = fetch_permission(worker.client, GITHUB_API, repo, &login, worker.token).await.ok();
    let room = OwnedRoomId::try_from(watched.watch.setting.room_id.as_str()).map_err(|_| format!("Invalid room {}", watched.watch.setting.room_id))?;
    let peer = pick_peer(worker, &room).await;
    let io = LiveTrack { worker, watched, repo, room, login, peer, permission };
    Ok(tracker::follow(&io, &Inputs { repo, now, tracks: &tracks, seen: watched.seen, open_issues: watched.open_issues, own_pulls: &own }).await)
}

/// Another active Agent of this owner that is in the room, to review.
async fn pick_peer(worker: &Worker<'_>, room: &OwnedRoomId) -> Option<OwnedUserId> {
    use matrix_sdk::ruma::events::room::member::MembershipState;
    let me = worker.agent["id"].as_str().unwrap_or_default();
    let joined = crate::sliding_sync::get_client()?.get_room(room)?;
    for peer in worker.peers.iter().filter(|p| p["id"].as_str().is_some_and(|id| id != me)) {
        let Ok(puppet) = OwnedUserId::try_from(peer["puppetMxid"].as_str().unwrap_or_default()) else { continue };
        let member = joined.get_member_no_sync(&puppet).await.ok().flatten();
        if member.is_some_and(|m| *m.membership() == MembershipState::Join) {
            return Some(puppet);
        }
    }
    None
}

struct LiveTrack<'a> {
    worker: &'a Worker<'a>,
    watched: &'a Watched<'a>,
    repo: &'a str,
    room: OwnedRoomId,
    login: String,
    peer: Option<OwnedUserId>,
    permission: Option<String>,
}

impl LiveTrack<'_> {
    fn agent_id(&self) -> &str {
        self.worker.agent["id"].as_str().unwrap_or_default()
    }
}

impl TrackIo for LiveTrack<'_> {
    async fn record(&self, change: Value) -> Result<(), String> {
        super::still_current(self.worker.epoch)?;
        super::live::saved(self.worker.service, self.agent_id(), Some(json!({"track": change}))).await.map(|_| ())
    }
    async fn say(&self, number: i64, key: &str, text: String) -> Result<(), String> {
        super::still_current(self.worker.epoch)?;
        if self.paused() {
            return Err(crate::i18n::tr("Agent paused by the owner").to_owned());
        }
        let mut extra = Map::new();
        extra.insert(MARKER.into(), json!({"repo": self.repo, "number": number, "step": key}));
        let txn = line_txn_id(self.agent_id(), self.repo, number, key);
        crate::hagency::room_send::send_routed_with(self.room.clone(), RoomMessageEventContent::notice_plain(text), extra, txn).await
    }
    async fn hand_off(&self, issue: &Issue, attempt: u32) -> Result<(), String> {
        if let Some(entry) = self.watched.entry {
            // The claim of the first attempt normally still holds (24 h); one
            // held by another Agent stops the retry.
            if super::work::claim(self.worker, entry, issue.number, &self.login, true).await? != Holder::Ours {
                return Err(crate::i18n::tr("claimed by another Agent").to_owned());
            }
        }
        let auto_merge = self.watched.entry.is_some() || super::live::auto_merge(self.agent_id()).await;
        let live = super::live::Live::new(self.worker.service, self.worker.agent, self.watched.watch, auto_merge, self.worker.epoch)?.with_attempt(attempt);
        let outcome = super::handoff::hand_off(&live, &[issue]).await;
        outcome.error.map_or(Ok(()), Err)
    }
    async fn closed_state(&self, number: i64) -> Result<IssueFact, String> {
        let (state, reason) = fetch_issue_state(self.worker.client, GITHUB_API, self.repo, number, self.worker.token).await.map_err(|e| e.to_string())?;
        Ok(match (state.as_str(), reason.as_str()) {
            ("open", _) => IssueFact::Open,
            ("closed", "completed") => IssueFact::Done,
            ("closed", _) => IssueFact::ClosedOther,
            _ => return Err(format!("unknown issue state {state:?}")),
        })
    }
    async fn cannot_fix(&self, track: &Track) -> Result<bool, String> {
        let comments = fetch_comments(self.worker.client, GITHUB_API, self.repo, track.number, self.worker.token).await.map_err(|e| e.to_string())?;
        Ok(says_cannot_fix(&comments, &self.login, track.handed_at - CLOCK_SLACK_SECS))
    }
    async fn follow_pr(&self, pr: &PullRequest) -> Result<Followed, String> {
        if pr.draft {
            return Ok(Followed::Draft);
        }
        let Some(peer) = &self.peer else { return Ok(Followed::NeedsPerson) };
        let io = LiveLoop::new(self.worker, &self.watched.watch.setting.room_id)?;
        let ctx = LoopCtx { agent: peer, maintainer: review_loop::can_merge(self.permission.as_deref()), permission: self.permission.as_deref() };
        review_loop::advance(&io, &review_loop::target_of(self.repo, pr), &ctx).await?;
        Ok(Followed::Requested)
    }
    fn paused(&self) -> bool {
        crate::hagency::kill_switch::paused_here(self.agent_id())
    }
}

/// Fixed by Agent, repository, issue and step: one line per step.
pub(super) fn line_txn_id(agent_id: &str, repo: &str, number: i64, key: &str) -> OwnedTransactionId {
    let hash = blake3::hash(format!("{agent_id}\n{repo}\n{number}\n{key}").as_bytes());
    OwnedTransactionId::from(format!("hagency-track-{}", hash.to_hex()))
}
