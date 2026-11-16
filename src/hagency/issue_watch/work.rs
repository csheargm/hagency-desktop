//! The Agent's work list on the poller (ADR 0016): every repository the owner
//! assigned, each with its role. A developer claims new issues on GitHub (a
//! claim comment; the earliest unexpired claim holds) and hands them to the
//! Agent; a reviewer or maintainer runs the review loop on other people's
//! pull requests. One repository's failure is reported and never stops the
//! others. GitHub is the record; the owner's token is the only identity used.
use super::handoff::{self, Handoff};
use super::loop_notice::LoopNotice;
use super::review_loop::{self, LoopCtx, LoopIo};
use crate::hagency::backend::Service;
use ::hagency::issue_watch::{
    CLAIM_MARKER, Client, GITHUB_API, Issue, IssueWatch, WatchMode, WatchSetting, claim_holder, fetch_comments, fetch_login, fetch_open_issues,
    fetch_open_pulls, fetch_permission, others_ready_pulls, post_comment, select,
};
use ::hagency::issue_watch::{IssueFilter, OwnerRules, Role};
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use serde_json::{Value, json};
use std::collections::HashSet;

/// What one pass over the work list did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkOutcome {
    pub handed: usize,
    /// One line per repository that failed: "owner/name: why".
    pub errors: Vec<String>,
}

/// Everything one pass needs about the Agent.
pub(crate) struct Worker<'a> {
    pub service: &'a Service,
    pub client: &'a Client,
    pub agent: &'a Value,
    pub token: &'a str,
    pub epoch: u64,
}

/// Run every repository of `saved["work"]`.
pub(crate) async fn run(worker: &Worker<'_>, saved: &Value) -> WorkOutcome {
    let mut outcome = WorkOutcome::default();
    let work: Vec<OwnerRules> = match serde_json::from_value(saved["work"].clone()) {
        Ok(work) => work,
        Err(error) => return WorkOutcome { handed: 0, errors: vec![format!("Unreadable work list: {error}")] },
    };
    if work.is_empty() {
        return outcome;
    }
    let login = match fetch_login(worker.client, GITHUB_API, worker.token).await {
        Ok(login) => login,
        Err(error) => return WorkOutcome { handed: 0, errors: vec![error.to_string()] },
    };
    for entry in &work {
        let seen: HashSet<i64> = saved["workSeen"][&entry.repo].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
        let done = match entry.role {
            Role::Developer => develop(worker, entry, &login, &seen).await,
            Role::Reviewer | Role::Maintainer => review(worker, entry, &login).await.map(|_| 0),
        };
        match done {
            Ok(handed) => outcome.handed += handed,
            Err(error) => outcome.errors.push(format!("{}: {error}", entry.repo)),
        }
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
    let left = loop_api(worker.service, id, json!({})).await?["claimsLeft"].as_u64().unwrap_or(0) as usize;
    let picks: Vec<&Issue> = select(&watch, &issues, seen).into_iter().take(left).collect();
    // The work list always carries the pull request on past the draft.
    let live = super::live::Live::new(worker.service, worker.agent, &watch, true, worker.epoch)?;
    let mut handed = 0;
    for issue in picks {
        if live.paused() {
            return Err(crate::i18n::tr("Agent paused by the owner").to_owned());
        }
        if !claim(worker, entry, issue.number, login).await? {
            // Someone else holds it: never handed off, never retried.
            live.mark_seen(issue.number).await?;
            continue;
        }
        let outcome = handoff::hand_off(&live, &[issue]).await;
        if let Some(error) = outcome.error {
            return Err(error);
        }
        loop_api(worker.service, id, json!({"workClaim": {"repo": entry.repo, "number": issue.number}})).await?;
        handed += outcome.handed;
    }
    Ok(handed)
}

/// Claim issue `number` on GitHub: post the claim unless one holds, then
/// re-read so two hosts racing agree on the earliest. True when it is ours.
async fn claim(worker: &Worker<'_>, entry: &OwnerRules, number: i64, login: &str) -> Result<bool, String> {
    let read = || async { fetch_comments(worker.client, GITHUB_API, &entry.repo, number, worker.token).await.map_err(|e| e.to_string()) };
    let now = chrono::Utc::now().timestamp();
    if let Some(holder) = claim_holder(&read().await?, now) {
        return Ok(holder.eq_ignore_ascii_case(login));
    }
    let label = worker.agent["displayName"].as_str().unwrap_or("Agent");
    post_comment(worker.client, GITHUB_API, &entry.repo, number, worker.token, &format!("{CLAIM_MARKER} — {}", super::notice::quoted_title(label))).await.map_err(|e| e.to_string())?;
    Ok(claim_holder(&read().await?, now).is_some_and(|holder| holder.eq_ignore_ascii_case(login)))
}

/// Reviewer or maintainer: one loop step per other person's ready pull request.
async fn review(worker: &Worker<'_>, entry: &OwnerRules, login: &str) -> Result<(), String> {
    let pulls = fetch_open_pulls(worker.client, GITHUB_API, &entry.repo, worker.token).await.map_err(|e| e.to_string())?;
    let maintainer = entry.role == Role::Maintainer;
    let trusted = |author: &str| !maintainer || entry.trusted_authors.iter().any(|t| t == "*" || t.eq_ignore_ascii_case(author));
    let picks: Vec<_> = others_ready_pulls(&pulls, login, entry.since).into_iter().filter(|pr| trusted(&pr.author)).collect();
    if picks.is_empty() {
        return Ok(());
    }
    // Read with this owner's own token, never cached across owners.
    let permission = if maintainer { fetch_permission(worker.client, GITHUB_API, &entry.repo, login, worker.token).await.ok() } else { None };
    let agent = puppet(worker.agent)?;
    let io = LiveLoop::new(worker, &entry.room_id)?;
    let ctx = LoopCtx { agent: &agent, maintainer, permission: permission.as_deref() };
    for pr in picks {
        review_loop::advance(&io, &review_loop::target_of(&entry.repo, pr), &ctx).await?;
    }
    Ok(())
}

/// The owner API's `review` part for this Agent.
async fn loop_api(service: &Service, agent_id: &str, change: Value) -> Result<Value, String> {
    Ok(super::live::saved(service, agent_id, Some(json!({"review": change}))).await?["review"].clone())
}

/// The live loop: notices into the repository's room, the owner API, Stop All.
struct LiveLoop<'a> {
    worker: &'a Worker<'a>,
    agent_id: String,
    room_id: OwnedRoomId,
}
impl<'a> LiveLoop<'a> {
    fn new(worker: &'a Worker<'a>, room: &str) -> Result<Self, String> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| format!("Invalid room {room}"))?;
        Ok(Self { worker, agent_id: worker.agent["id"].as_str().unwrap_or_default().to_owned(), room_id })
    }
}
impl LoopIo for LiveLoop<'_> {
    async fn post(&self, notice: &LoopNotice) -> Result<(), String> {
        super::still_current(self.worker.epoch)?;
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
