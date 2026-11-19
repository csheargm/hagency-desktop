//! The tracker's live I/O (ADR 0020): GitHub reads with the owner's token
//! (never a write besides the existing claim comment), room lines as
//! `m.notice` (never dispatched as a request), hand-offs through `live::Live`,
//! pull request follow-through through the review loop (`review_loop`), and
//! the owner API's `track` record.
use super::merge_hold::{self, Gate};
use super::review_loop::{self, LoopCtx, LoopIo};
use super::reviewer_pick::{Peers, Pick};
use super::tracker::{self, CLOCK_SLACK_SECS, Followed, Followups, Inputs, TrackIo};
use super::work::{Holder, LiveLoop, Worker};
use super::run_end::RunView;
use ::hagency::issue_watch::issue_track::{IssueFact, Track, says_cannot_fix};
use ::hagency::issue_watch::{GITHUB_API, Issue, IssueWatch, OwnerRules, PullRequest, fetch_comments, fetch_open_issues, fetch_reviews, fetch_branches, fetch_pull_merged, fetch_issue_state, fetch_login, fetch_open_pulls, fetch_permission};
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::{OwnedRoomId, OwnedTransactionId, OwnedUserId};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

/// The content field that marks a tracker line.
pub const MARKER: &str = "io.hagency.issue_track";

/// One watched repository as the check sees it.
pub(crate) struct Watched<'a> {
    pub watch: &'a IssueWatch,
    /// The work-list entry, when the repository comes from the work list (claims apply).
    pub entry: Option<&'a OwnerRules>,
    pub open_issues: &'a [Issue],
    pub seen: &'a HashSet<i64>,
}

impl Watched<'_> {
    /// The work-list entry's start; a single-repository watch has no cutoff.
    fn since(&self) -> i64 {
        self.entry.map_or(i64::MIN, |e| e.since)
    }
}

/// Whether `repo` is on the Agent's work list (`saved["work"]`).
pub(crate) fn on_work_list(saved: &Value, repo: &str) -> bool {
    saved["work"].as_array().into_iter().flatten().any(|e| e["repo"].as_str().is_some_and(|r| r.eq_ignore_ascii_case(repo)))
}

/// Follow every tracked issue of one repository; failures become errors in the result.
pub(crate) async fn follow_repo(worker: &Worker<'_>, watched: &Watched<'_>, saved: &Value) -> Followups {
    match follow_inner(worker, watched, saved).await {
        Ok(followups) => followups,
        Err(error) => Followups { errors: vec![format!("{}: {error}", watched.watch.setting.repo)], ..Default::default() },
    }
}

/// A work-list repository, which the work list follows (its claims apply)
/// even when the single-repository watch names it too (GPT review).
pub(crate) async fn follow_work(worker: &Worker<'_>, entry: &OwnerRules, watch: &IssueWatch, saved: &Value, seen: &HashSet<i64>) -> Followups {
    let issues = match fetch_open_issues(worker.client, GITHUB_API, &entry.repo, worker.token).await {
        Ok(issues) => issues,
        Err(error) => return Followups { errors: vec![format!("{}: {error}", entry.repo)], ..Default::default() },
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
    // Before any other read can fail, so the room's copy is filtered either way (GPT review).
    super::track_age::look_up(worker, repo, tracks.iter().filter(ours), watched.open_issues, watched.since()).await;
    let now = chrono::Utc::now().timestamp();
    if !busy {
        // Only finished records: their lines need no GitHub read. The room's
        // copy is still brought up to date: a final write that failed is
        // retried here (a no-op without network once it went through, GPT review).
        let room = OwnedRoomId::try_from(watched.watch.setting.room_id.as_str()).map_err(|_| format!("Invalid room {}", watched.watch.setting.room_id))?;
        let shown = tracks.iter().filter(ours).filter(|t| !super::track_age::hidden(t, watched.open_issues, watched.since()));
        let mut done = Followups { lines: shown.filter_map(tracker::terminal_line).collect(), ..Default::default() };
        if let Err(error) = share(worker, watched, &room, &[], &[]).await {
            done.errors.push(error);
        }
        return Ok(done);
    }
    let login = fetch_login(worker.client, GITHUB_API, worker.token).await.map_err(|e| e.to_string())?;
    let pulls = fetch_open_pulls(worker.client, GITHUB_API, repo, worker.token).await.map_err(|e| e.to_string())?;
    let own: Vec<PullRequest> = pulls.into_iter().filter(|p| p.author.eq_ignore_ascii_case(&login)).collect();
    // A failed read is kept as the failure: the merge step counts it, never "cannot merge" (GPT review).
    let permission = fetch_permission(worker.client, GITHUB_API, repo, &login, worker.token).await.map_err(|e| e.to_string());
    let room = OwnedRoomId::try_from(watched.watch.setting.room_id.as_str()).map_err(|_| format!("Invalid room {}", watched.watch.setting.room_id))?;
    let peers = super::reviewer_candidates::peers(worker, &room).await;
    let io = LiveTrack { worker, watched, repo, room: room.clone(), login, peer: peers.pick(&[]), peers, permission };
    let mut followed = tracker::follow(&io, &Inputs { repo, now, tracks: &tracks, seen: watched.seen, since: watched.since(), open_issues: watched.open_issues, own_pulls: &own }).await;
    if let Err(error) = share(worker, watched, &room, &followed.holds, &followed.predating).await {
        followed.errors.push(error);
    }
    Ok(followed)
}

/// The room's copy of this repository's tracker states (ADR 0023), read
/// back from the owner API after this check's changes, without the issues
/// the tracker leaves alone as older than the work-list entry.
async fn share(worker: &Worker<'_>, watched: &Watched<'_>, room: &OwnedRoomId, holds: &[(i64, String)], predating: &[i64]) -> Result<(), String> {
    let agent = worker.agent["id"].as_str().unwrap_or_default();
    let mxid = worker.agent["puppetMxid"].as_str().ok_or("Unreadable Agent Matrix ID")?;
    let saved = super::live::saved(worker.service, agent, None).await?;
    let tracks: Vec<Track> = if saved["track"].is_null() { Vec::new() } else { serde_json::from_value(saved["track"].clone()).map_err(|e| format!("Unreadable issue tracks: {e}"))? };
    let titles = watched.open_issues.iter().map(|i| (i.number, i.title.clone())).collect();
    let now = chrono::Utc::now().timestamp();
    let tracks = super::status_publish::shown(tracks, watched.open_issues, watched.since(), predating);
    let fresh = super::status_publish::entries(mxid, &watched.watch.setting.repo, &tracks, &titles, holds, now)?;
    super::status_publish::publish(room, mxid, &watched.watch.setting.repo, fresh, now).await
}

struct LiveTrack<'a> {
    worker: &'a Worker<'a>,
    watched: &'a Watched<'a>,
    repo: &'a str,
    room: OwnedRoomId,
    login: String,
    /// The room's pick, for its once-said fallback line; each pull request
    /// picks again without the reviewers that never answered its head.
    peer: Option<Pick>,
    peers: Peers,
    permission: Result<String, String>,
}

impl LiveTrack<'_> {
    fn agent_id(&self) -> &str {
        self.worker.agent["id"].as_str().unwrap_or_default()
    }
    /// The gate's answer for `pr` and the step it leads to (a `settled` head is never re-asked); every GitHub read
    /// (the owner API's own, and the reviews) fails here, never silently.
    async fn merge_step(&self, io: &LiveLoop<'_>, target: &super::loop_notice::Target, (pr, peer, settled): (&PullRequest, Option<&Pick>, bool), fixer: &OwnedUserId) -> Result<Gate, String> {
        let permission = self.permission.as_deref().map_err(|e| format!("GitHub permission: {e}"))?;
        let ctx = &LoopCtx { agent: fixer, name: &self.agent_label(), maintainer: true, permission: Some(permission) };
        let readiness = io.review(review_loop::readiness_ask(io, self.repo, pr.number)).await?["readiness"].clone();
        review_loop::peer_review::note_review(&io.room(), target, &readiness);
        // Only a missing approval waits on the reviewer; GitHub says whether it answered.
        if readiness["kind"] != "approvals" || !readiness["head"].is_string() {
            let own = super::approval_note::reviewer_of(&self.peers, super::approval_note::asked_again(peer), &self.room).await;
            return merge_hold::follow_gate(io, target, &super::approval_note::own_reviewer(readiness, own.as_ref()), ctx, false).await;
        }
        let reviews = fetch_reviews(self.worker.client, GITHUB_API, self.repo, pr.number, self.worker.token).await.map_err(|e| e.to_string())?;
        let now = chrono::Utc::now().timestamp();
        // A verdict on GitHub the gate never recorded: ask the reviewer again before holding (PR #105).
        if let Some(peer) = super::approval_note::asked_again(peer).filter(|_| !settled) && super::review_again::asks_again(io, target, (&readiness, &reviews, &self.login), &peer.reviewer, now).await? {
            return Ok(Gate::WaitReview);
        }
        let reviewer = super::approval_note::reviewer_of(&self.peers, peer, &self.room).await;
        let readiness = super::approval_note::for_hold(readiness, (&reviews, &self.login), reviewer.as_ref());
        let seen_at = merge_hold::lock_clock().seen_at(&pr.url, &pr.head_sha, now);
        let settled = merge_hold::review_settled(&reviews, &pr.head_sha, pr.created_at, seen_at, now);
        merge_hold::follow_gate(io, target, &readiness, ctx, settled).await
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
    async fn hand_off(&self, issue: &Issue, attempt: u32, branch: Option<&str>, again: u32) -> Result<bool, String> {
        if let Some(entry) = self.watched.entry {
            // The claim of the first attempt normally still holds (24 h).
            if super::work::claim(self.worker, entry, issue.number, &self.login, true).await? != Holder::Ours {
                return Ok(false);
            }
        }
        let auto_merge = self.watched.entry.is_some() || super::live::auto_merge(self.agent_id()).await;
        let live = super::live::Live::new(self.worker.service, self.worker.agent, self.watched.watch, auto_merge, self.worker.epoch)?.with_attempt(attempt).with_branch(branch).with_again(again);
        let outcome = super::handoff::hand_off(&live, &[issue]).await;
        outcome.error.map_or(Ok(true), Err)
    }
    async fn run_end(&self, track: &Track) -> Result<RunView, String> {
        let ask = crate::hagency::model::Command::Local { agent_id: self.agent_id().into(), action: crate::hagency::model::LocalAction::RecentActivity, input: None, binding_id: None, requester: None };
        let activity = self.worker.service.execute(ask).await?;
        Ok(super::run_end::run_end_in(&activity, (crate::sliding_sync::current_user_id().ok_or("Signed out")?.as_str(), self.room.as_str()), track))
    }
    async fn pushed_branch(&self, number: i64) -> Result<Option<String>, String> {
        let branches = fetch_branches(self.worker.client, GITHUB_API, self.repo, self.worker.token).await.map_err(|e| e.to_string())?;
        Ok(super::run_end::issue_branch(&branches, number))
    }
    fn agent_label(&self) -> String {
        self.worker.agent["displayName"].as_str().filter(|n| !n.is_empty()).unwrap_or("The Agent").to_owned()
    }
    async fn pr_merged(&self, pr: u64) -> Result<Option<bool>, String> {
        Ok(fetch_pull_merged(self.worker.client, GITHUB_API, self.repo, pr, self.worker.token).await.map_err(|e| e.to_string())?.map(|by| by.eq_ignore_ascii_case(&self.login)))
    }
    async fn merged_linking_pr(&self, track: &Track) -> Result<Option<u64>, String> {
        super::merged_link::find(self.worker, (self.repo, &self.login), track).await
    }
    async fn closed_state(&self, number: i64) -> Result<(IssueFact, Option<Issue>), String> {
        let (state, reason, issue) = fetch_issue_state(self.worker.client, GITHUB_API, self.repo, number, self.worker.token).await.map_err(|e| e.to_string())?;
        let fact = match (state.as_str(), reason.as_str()) {
            ("open", _) => IssueFact::Open,
            ("closed", "completed") => IssueFact::Done,
            ("closed", _) => IssueFact::ClosedOther,
            _ => return Err(format!("unknown issue state {state:?}")),
        };
        Ok((fact, issue))
    }
    async fn cannot_fix(&self, track: &Track) -> Result<bool, String> {
        let comments = fetch_comments(self.worker.client, GITHUB_API, self.repo, track.number, self.worker.token).await.map_err(|e| e.to_string())?;
        Ok(says_cannot_fix(&comments, &self.login, track.handed_at - CLOCK_SLACK_SECS))
    }
    async fn follow_pr(&self, pr: &PullRequest) -> Result<Followed, String> {
        // A ready request run after a newer push waits like a draft (round-2 GPT review).
        if pr.draft || super::review_again::ready_moved(&pr.url, &pr.head_sha, pr.draft) {
            return Ok(Followed::Draft);
        }
        let io = LiveLoop::new(self.worker, &self.watched.watch.setting.room_id)?;
        let target = review_loop::target_of(self.repo, pr);
        let passed = review_loop::passed_reviewers(&io, &target).await?;
        let peer = self.peers.pick(&passed);
        if peer.is_none() && passed.is_empty() {
            return Ok(Followed::NeedsPerson);
        }
        // The peer only reviews. The merge step is judged and addressed for
        // this Agent, whose ledger, token and permission the gate reads (GPT review).
        let settled = review_loop::peer_review::advance(&io, &target, peer.as_ref(), &review_loop::peer_review::LiveFacts { worker: self.worker, peers: &self.peers, room: &self.room }).await?;
        // Told ready at this head and level, and still allowed to merge: nothing more is said; a revoked permission goes the full gate (GPT review).
        if settled && super::tracker_lines::still_ready(&self.permission) { return Ok(Followed::AwaitsPerson) }
        let fixer = OwnedUserId::try_from(self.worker.agent["puppetMxid"].as_str().unwrap_or_default()).map_err(|_| "Unreadable Agent Matrix ID".to_owned())?;
        // Everyone passed over: nobody is asked, but a late verdict still merges (GPT review).
        let gate = match self.merge_step(&io, &target, (pr, peer.as_ref(), settled), &fixer).await {
            Ok(Gate::Unreadable(error)) | Err(error) => Err(error),
            Ok(gate) => Ok(gate),
        };
        Ok(match gate {
            Ok(gate) => {
                merge_hold::lock_streaks().reached(&pr.url);
                followed_of(gate, peer.is_some())
            }
            Err(error) => {
                // A transport or timeout failure is never swallowed: logged,
                // shown, retried next check, and told once after a streak.
                let failures = merge_hold::lock_streaks().failed(&pr.url);
                makepad_widgets::log!("WARN issue_track_merge_step_failed pr={} failures={failures}: {error}", pr.url);
                if let Err(notice) = merge_hold::after_failure(&io, &target, failures).await {
                    // The status line below still shows the streak; the notice retries next check.
                    makepad_widgets::log!("WARN issue_track_unreachable_notice_failed pr={}: {notice}", pr.url);
                }
                Followed::Unreachable(failures)
            }
        })
    }
    async fn ask_ready(&self, pr: &PullRequest) -> Result<(), String> {
        let fixer = OwnedUserId::try_from(self.worker.agent["puppetMxid"].as_str().unwrap_or_default()).map_err(|_| "Unreadable Agent Matrix ID".to_owned())?;
        let io = LiveLoop::new(self.worker, &self.watched.watch.setting.room_id)?;
        super::review_again::ask_ready(&io, &review_loop::target_of(self.repo, pr), &fixer, &self.agent_label()).await
    }
    fn reviewer_notice(&self) -> Option<String> {
        self.peer.as_ref().and_then(|p| p.notice.clone())
    }
    fn paused(&self) -> bool {
        crate::hagency::kill_switch::paused_here(self.agent_id())
    }
}

/// What the merge step's `gate` means for the pull request; `asking`: a reviewer was asked,
/// else everyone was passed over and a person reviews, though a late verdict still carries it on (GPT review).
pub(super) fn followed_of(gate: Gate, asking: bool) -> Followed {
    match gate {
        Gate::Merge => Followed::Merging,
        Gate::WaitCi => Followed::WaitingCi,
        Gate::Hold(missing) => if merge_hold::only_assist(&missing) { Followed::AwaitsPerson } else { Followed::Held(missing.into_iter().map(|(m, _)| m).collect()) },
        Gate::WaitReview | Gate::WaitFacts | Gate::Unreadable(_) if !asking => Followed::NeedsPerson,
        Gate::WaitReview | Gate::WaitFacts | Gate::Unreadable(_) => Followed::Requested,
    }
}

/// Fixed by Agent, repository, issue and step: one line per step.
pub(super) fn line_txn_id(agent_id: &str, repo: &str, number: i64, key: &str) -> OwnedTransactionId {
    let hash = blake3::hash(format!("{agent_id}\n{repo}\n{number}\n{key}").as_bytes());
    OwnedTransactionId::from(format!("hagency-track-{}", hash.to_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GPT review: with every reviewer passed over the head was never judged
    /// again, so a late approval left it waiting; the gate still runs.
    #[test]
    fn with_everyone_passed_over_a_late_approval_still_merges() {
        assert_eq!(followed_of(Gate::Merge, false), Followed::Merging);
        assert_eq!(followed_of(Gate::WaitReview, false), Followed::NeedsPerson, "nobody left to ask: a person reviews");
        assert_eq!(followed_of(Gate::WaitReview, true), Followed::Requested);
        assert_eq!(followed_of(Gate::WaitCi, false), Followed::WaitingCi);
    }
}
