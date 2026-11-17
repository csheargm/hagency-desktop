//! The Agent's work list on the poller (ADR 0016, 0017): every repository the
//! owner listed; there are no roles. In each, the Agent claims new issues on GitHub (a
//! claim comment; the earliest unexpired claim holds) and hands them to the
//! Agent, and runs the review loop on other people's pull requests (merging
//! where the owner's GitHub permission allows); the owner's own pull requests
//! go to the merge step once another account approved their exact head. One repository's failure is reported and never stops the
//! others. GitHub is the record; the owner's token is the only identity used.
use super::handoff::{self, Handoff};
use super::loop_notice::LoopNotice;
use super::review_loop::{self, LoopCtx, LoopIo};
use crate::hagency::backend::Service;
use ::hagency::issue_watch::{
    Client, Comment, GITHUB_API, Issue, IssueWatch, WatchMode, WatchSetting, claim_authors, claim_body, claim_holder, fetch_comments, fetch_login, fetch_open_issues,
    fetch_open_pulls, fetch_permission, others_ready_pulls, own_ready_pulls, approved_by_other, fetch_reviews, PullRequest, post_comment, select,
};
use ::hagency::issue_watch::{IssueFilter, OwnerRules};
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use serde_json::{Value, json};
use std::collections::HashSet;

/// What one pass over the work list did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkOutcome {
    pub handed: usize,
    /// One line per repository that failed: "owner/name: why".
    pub errors: Vec<String>,
    /// The tracker's status parts ("#33 retrying (2/3)", ADR 0018).
    pub tracked: Vec<String>,
}

/// Everything one pass needs about the Agent.
pub(crate) struct Worker<'a> {
    pub service: &'a Service,
    pub client: &'a Client,
    pub agent: &'a Value,
    pub token: &'a str,
    pub epoch: u64,
    /// Every active Agent of this owner: the tracker asks another one to review.
    pub peers: &'a [Value],
}

/// Run every repository of `saved["work"]`.
pub(crate) async fn run(worker: &Worker<'_>, saved: &Value) -> WorkOutcome {
    let mut outcome = WorkOutcome::default();
    let work: Vec<OwnerRules> = match serde_json::from_value(saved["work"].clone()) {
        Ok(work) => work,
        Err(error) => return WorkOutcome { errors: vec![format!("Unreadable work list: {error}")], ..Default::default() },
    };
    if work.is_empty() {
        return outcome;
    }
    let login = match fetch_login(worker.client, GITHUB_API, worker.token).await {
        Ok(login) => login,
        Err(error) => return WorkOutcome { errors: vec![error.to_string()], ..Default::default() },
    };
    for entry in &work {
        let seen: HashSet<i64> = saved["workSeen"][&entry.repo].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
        // No roles (ADR 0017): every repository is both fixed and reviewed,
        // each step on its own so one failing never stops the other (GPT review P1).
        match develop(worker, entry, &login, &seen).await {
            Ok(handed) => outcome.handed += handed,
            Err(error) => outcome.errors.push(format!("{}: {error}", entry.repo)),
        }
        if let Err(error) = review(worker, entry, &login).await {
            outcome.errors.push(format!("{}: {error}", entry.repo));
        }
        let followed = super::tracker_live::follow_work(worker, entry, &watch_for(entry), saved, &seen).await;
        outcome.tracked.extend(followed.lines);
        outcome.errors.extend(followed.errors);
    }
    outcome
}

fn watch_for(entry: &OwnerRules) -> IssueWatch {
    let mode = match entry.issues { IssueFilter::Trusted => WatchMode::Trusted, IssueFilter::All => WatchMode::All };
    IssueWatch { setting: WatchSetting { mode, label: String::new(), repo: entry.repo.clone(), room_id: entry.room_id.clone(), push_trusted: false }, enabled_at: entry.since }
}

fn puppet(agent: &Value) -> Result<OwnedUserId, String> {
    let mxid = agent["puppetMxid"].as_str().unwrap_or_default();
    OwnedUserId::try_from(mxid).map_err(|_| format!("Unreadable Agent Matrix ID {mxid}"))
}

/// Developer: claim and hand off new issues, within the Agent's daily limit.
async fn develop(worker: &Worker<'_>, entry: &OwnerRules, login: &str, seen: &HashSet<i64>) -> Result<usize, String> {
    let id = worker.agent["id"].as_str().unwrap_or_default();
    let watch = watch_for(entry);
    let issues = fetch_open_issues(worker.client, GITHUB_API, &entry.repo, worker.token).await.map_err(|e| e.to_string())?;
    let mut slots = super::work_slots::open_slots(worker, entry, login, seen, &issues).await?;
    if slots.is_full() {
        return Ok(0); // claims resume once one of them merges or closes
    }
    // The daily limit is enforced by each reservation below, which also lets
    // an issue reserved by a pass whose hand-off failed be handed off now.
    let picks: Vec<&Issue> = select(&watch, &issues, seen);
    // The work list always carries the pull request on past the draft.
    let live = super::live::Live::new(worker.service, worker.agent, &watch, true, worker.epoch)?;
    let mut handed = 0;
    for issue in picks {
        // Each hand-off becomes an open pull request: one slot each, so a
        // single pass never opens more than the cap (GPT review P1).
        if slots.is_full() {
            break;
        }
        if live.paused() {
            return Err(crate::i18n::tr("Agent paused by the owner").to_owned());
        }
        // Held by another eligible claimant: skipped, never marked seen, so
        // it is tried again once that claim expires (GPT review, Nov 16).
        if claim(worker, entry, issue.number, login, false).await? == Holder::Other {
            continue;
        }
        // Reserved next, so the daily limit holds before anything is posted
        // on GitHub, and a reserved issue whose hand-off failed is retried.
        let reserved = loop_api(worker.service, id, json!({"workClaim": {"repo": entry.repo, "number": issue.number}})).await?;
        if reserved["claimed"] != true {
            break; // the daily limit is used up
        }
        if claim(worker, entry, issue.number, login, true).await? != Holder::Ours {
            continue; // lost a race; retried after that claim expires
        }
        let outcome = handoff::hand_off(&live, &[issue]).await;
        if let Some(error) = outcome.error {
            return Err(error);
        }
        handed += outcome.handed;
        slots.take(outcome.handed);
    }
    Ok(handed)
}

/// Who holds an issue, as this Agent sees it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Holder {
    Ours,
    Other,
    Nobody,
}

/// The repository permissions whose claims count (besides this login's).
const CLAIM_PERMISSIONS: [&str; 4] = ["admin", "maintain", "write", "triage"];

/// Who holds issue `number`; with `post`, claim it first when nobody does,
/// then re-read so two hosts racing agree on the earliest. A claim counts
/// only from this login (then it must name this Agent's id to be ours) or
/// from a login with write or triage rights on the repository.
pub(super) async fn claim(worker: &Worker<'_>, entry: &OwnerRules, number: i64, login: &str, post: bool) -> Result<Holder, String> {
    let id = worker.agent["id"].as_str().unwrap_or_default();
    let now = chrono::Utc::now().timestamp();
    let comments = read_comments(worker, entry, number).await?;
    let holder = holder_of(worker, entry, &comments, login, id, now).await?;
    if !post || holder != Holder::Nobody {
        return Ok(holder);
    }
    if crate::hagency::kill_switch::paused_here(id) {
        return Err(crate::i18n::tr("Agent paused by the owner").to_owned());
    }
    let label = worker.agent["displayName"].as_str().unwrap_or("Agent");
    post_comment(worker.client, GITHUB_API, &entry.repo, number, worker.token, &claim_body(id, &super::notice::quoted_title(label))).await.map_err(|e| e.to_string())?;
    let comments = read_comments(worker, entry, number).await?;
    holder_of(worker, entry, &comments, login, id, now).await
}

async fn read_comments(worker: &Worker<'_>, entry: &OwnerRules, number: i64) -> Result<Vec<Comment>, String> {
    fetch_comments(worker.client, GITHUB_API, &entry.repo, number, worker.token).await.map_err(|e| e.to_string())
}

/// An unreadable permission fails the pass (retried later) rather than
/// dropping a claim that may be real (GPT review, Nov 16).
async fn holder_of(worker: &Worker<'_>, entry: &OwnerRules, comments: &[Comment], login: &str, id: &str, now: i64) -> Result<Holder, String> {
    let mut team = vec![login.to_owned()];
    for author in claim_authors(comments, login, now) {
        let permission = match fetch_permission(worker.client, GITHUB_API, &entry.repo, author, worker.token).await {
            Ok(permission) => permission,
            // Not a collaborator at all: GitHub answers 404, no rights.
            Err(::hagency::issue_watch::FetchError::Status { status: 404, .. }) => String::new(),
            Err(error) => return Err(error.to_string()),
        };
        if CLAIM_PERMISSIONS.contains(&permission.as_str()) {
            team.push(author.to_owned());
        }
    }
    Ok(match claim_holder(comments, now, |a| team.iter().any(|t| t.eq_ignore_ascii_case(a))) {
        Some(claim) if claim.is(login, id) => Holder::Ours,
        Some(_) => Holder::Other,
        None => Holder::Nobody,
    })
}

/// Reviewer or maintainer: one loop step per other person's ready pull request.
async fn review(worker: &Worker<'_>, entry: &OwnerRules, login: &str) -> Result<(), String> {
    let pulls = fetch_open_pulls(worker.client, GITHUB_API, &entry.repo, worker.token).await.map_err(|e| e.to_string())?;
    let trusted = |author: &str| entry.trusted_authors.iter().any(|t| t == "*" || t.eq_ignore_ascii_case(author));
    let picks = others_ready_pulls(&pulls, login, entry.since);
    let own = own_ready_pulls(&pulls, login, entry.since);
    if picks.is_empty() && own.is_empty() {
        return Ok(());
    }
    // Merging follows the owner's own GitHub permission, read with the
    // owner's own token and never cached across owners.
    let permission = fetch_permission(worker.client, GITHUB_API, &entry.repo, login, worker.token).await.ok();
    let can_merge = review_loop::can_merge(permission.as_deref());
    let agent = puppet(worker.agent)?;
    let io = LiveLoop::new(worker, &entry.room_id)?;
    for pr in picks {
        // Anyone's pull request is reviewed; only a trusted author's goes on to a merge.
        let ctx = LoopCtx { agent: &agent, maintainer: can_merge && trusted(&pr.author), permission: permission.as_deref() };
        review_loop::advance(&io, &review_loop::target_of(&entry.repo, pr), &ctx).await?;
    }
    if can_merge {
        merge_own(worker, entry, &own, &io, &LoopCtx { agent: &agent, maintainer: true, permission: permission.as_deref() }).await?;
    }
    Ok(())
}

/// The owner's own pull requests (from claimed or planned issues) go to the
/// merge step only once another account approves exactly their head; the
/// merge gate (CI, policy, approvals) still judges them as any other.
async fn merge_own(worker: &Worker<'_>, entry: &OwnerRules, own: &[&PullRequest], io: &LiveLoop<'_>, ctx: &LoopCtx<'_>) -> Result<(), String> {
    for pr in own {
        let reviews = fetch_reviews(worker.client, GITHUB_API, &entry.repo, pr.number, worker.token).await.map_err(|e| e.to_string())?;
        if approved_by_other(&reviews, &pr.author, &pr.head_sha) {
            review_loop::advance_merge(io, &review_loop::target_of(&entry.repo, pr), ctx).await?;
        }
    }
    Ok(())
}

/// The owner API's `review` part for this Agent.
async fn loop_api(service: &Service, agent_id: &str, change: Value) -> Result<Value, String> {
    Ok(super::live::saved(service, agent_id, Some(json!({"review": change}))).await?["review"].clone())
}

/// The live loop: notices into the repository's room, the owner API, Stop All.
pub(super) struct LiveLoop<'a> {
    worker: &'a Worker<'a>,
    agent_id: String,
    room_id: OwnedRoomId,
}
impl<'a> LiveLoop<'a> {
    pub(super) fn new(worker: &'a Worker<'a>, room: &str) -> Result<Self, String> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| format!("Invalid room {room}"))?;
        Ok(Self { worker, agent_id: worker.agent["id"].as_str().unwrap_or_default().to_owned(), room_id })
    }
}
impl LoopIo for LiveLoop<'_> {
    async fn post(&self, notice: &LoopNotice) -> Result<(), String> {
        super::still_current(self.worker.epoch)?;
        // Checked right before the send, after every await that led here.
        if self.paused() {
            return Err(crate::i18n::tr("Agent paused by the owner").to_owned());
        }
        crate::hagency::room_send::send_routed_with(self.room_id.clone(), notice.content(), notice.marker(), notice.txn_id(&self.agent_id)).await
    }
    async fn review(&self, change: Value) -> Result<Value, String> {
        super::still_current(self.worker.epoch)?;
        loop_api(self.worker.service, &self.agent_id, change).await
    }
    fn paused(&self) -> bool {
        crate::hagency::kill_switch::paused_here(&self.agent_id)
    }
}
