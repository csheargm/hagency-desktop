//! The host side of Codex's `watched_repo_step` Room MCP tool (ADR 0022,
//! "Codex"): the Room MCP endpoint hands each call to the turn that owns
//! it, which checks it with the ledger exactly as a Claude `Bash` call
//! ([`Ledger::trusted_publish`]) and runs the authorized step off the async
//! threads ([`codex_step::run`]). Every step of a turn is stopped when the
//! turn ends (Stop All included): [`Running`] holds their cancel guards.
use crate::watch_trust::{TrustedStep, codex_step, host_git::{CancelOnDrop, HostCtx}};
use crate::{Ledger, Scope, ToolProposal};
use serde_json::json;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

/// At most this many steps wait for the turn at once.
const QUEUED: usize = 4;
/// The most output one step returns to the model.
const MAX_OUTPUT_BYTES: usize = 8 * 1024;
/// Seconds an authorized proposal stays valid (as a native approval).
const PROPOSAL_LIFETIME_SECS: i64 = 300;

/// One tool call: its command line and where the answer goes.
pub(super) struct Call {
    pub command: String,
    pub reply: oneshot::Sender<(String, bool)>,
}
pub(super) type Sender = mpsc::Sender<Call>;
pub(super) fn channel() -> (Sender, mpsc::Receiver<Call>) {
    mpsc::channel(QUEUED)
}

/// Hand one admitted tool call to its turn (`sender`) and wait for the
/// answer, at most `wait`, or until the turn ends (`cancel`). `Err` is the
/// refusal reason the model sees.
pub(super) async fn forward(
    sender: Option<Sender>,
    arguments: &serde_json::Value,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    wait: std::time::Duration,
) -> Result<(String, bool), &'static str> {
    let command = codex_step::command_of(arguments).ok_or("arguments_invalid")?;
    let sender = sender.ok_or("repo_steps_unavailable")?;
    let (reply, answer) = oneshot::channel();
    sender.try_send(Call { command, reply }).map_err(|_| "repo_steps_busy")?;
    tokio::select! { biased;
        _ = cancel.changed() => Err("turn_ended"),
        answer = tokio::time::timeout(wait, answer) => answer.ok().and_then(Result::ok).ok_or("repo_step_unanswered"),
    }
}

/// At most this many steps of a turn run at once (GPT review).
const PARALLEL: usize = 2;

/// The cancel guards of a turn's running steps, dropped with the turn, and
/// the slots that bound how many run at once.
pub(super) struct Running {
    guards: Vec<CancelOnDrop>,
    slots: std::sync::Arc<tokio::sync::Semaphore>,
    /// Finished steps the merge gate held something under: (key, succeeded).
    outcomes: std::sync::Arc<std::sync::Mutex<Vec<(String, bool)>>>,
    /// Makes this turn's gate keys unique, across turns and restarts.
    nonce: String,
    next: u64,
}
impl Default for Running {
    fn default() -> Self {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos().to_string()).unwrap_or_default();
        Self { guards: Vec::new(), slots: std::sync::Arc::new(tokio::sync::Semaphore::new(PARALLEL)), outcomes: std::sync::Arc::default(), nonce, next: 0 }
    }
}
impl Running {
    /// Settle what the gate held under each finished step: a review counts,
    /// a request for changes withdraws, a merge claim is kept, only on success.
    pub(super) fn settle(&self, ledger: &mut Ledger) -> crate::Result<()> {
        let finished = std::mem::take(&mut *self.outcomes.lock().map_err(|_| crate::Error::Invalid("repo step outcomes"))?);
        for (key, succeeded) in finished {
            ledger.settle_withdrawal(&key, succeeded)?;
            ledger.settle_agent_review(&key, succeeded)?;
        }
        Ok(())
    }
    /// A fresh gate key for one step (a Codex key: no Claude call id settles it).
    fn key(&mut self, dispatch: &str) -> String {
        self.next += 1;
        format!("{}step:{}", crate::merge_policy::codex_review::KEY_PREFIX, json!([dispatch, self.nonce, self.next]))
    }
}

/// What one turn's steps are checked against.
pub(super) struct Turn<'a> {
    pub owner: &'a str,
    pub scope: &'a Scope,
    pub dispatch: &'a str,
    /// The Room workspace (the turn's `cwd`).
    pub workspace: &'a str,
    pub token: Option<String>,
    /// False where Codex runs no native commands (a shared project).
    pub enabled: bool,
}

/// Answer `call`: a PR review verdict goes through the merge gate first,
/// exactly as a Claude `Bash` call does (recorded on the exact head once it
/// ran); otherwise refused at once unless push trust covers it, else started.
pub(super) async fn start(ledger: &mut Ledger, turn: &Turn<'_>, call: Call, running: &mut Running) {
    let github = crate::merge_policy::github::GhCli { token: turn.token.clone() };
    start_with(ledger, turn, call, running, &github).await
}

/// [`start`] reading GitHub through `source` (tests give a stand-in).
pub(super) async fn start_with(ledger: &mut Ledger, turn: &Turn<'_>, call: Call, running: &mut Running, source: &impl crate::merge_policy::github::PrSource) {
    let refuse = |call: Call, why: &str| {
        let text = json!({"error": "not_card_free", "reason": why, "next": "run it as a normal command; it asks your owner"}).to_string();
        let _ = call.reply.send((text, true));
    };
    if !turn.enabled {
        return refuse(call, "this room's workspace runs no commands");
    }
    // Abandoned while queued (its turn ended, or the MCP wait expired):
    // nothing starts for a call nobody waits for (GPT review).
    if call.reply.is_closed() {
        return;
    }
    let Ok(slot) = running.slots.clone().try_acquire_owned() else { return refuse(call, "another step is still running; wait for it") };
    let Some(proposal) = proposal(ledger, turn, &call.command) else { return refuse(call, "this run cannot be checked") };
    let key = running.key(turn.dispatch);
    let step = match gated_review(ledger, turn, &proposal, &call.command, &key, source).await {
        Some(step) => step,
        None => match ledger.trusted_publish(turn.owner, &proposal, super::now()) {
            Ok(Some(step)) if step != TrustedStep::Allow => step,
            // Not a card-free step, or the ledger could not say: nothing runs.
            _ => return refuse(call, "outside what push freely covers here"),
        },
    };
    // Given up while the gate read GitHub: nothing starts (what it held never settles).
    if call.reply.is_closed() {
        return;
    }
    let outcomes = running.outcomes.clone();
    let repo = match &step { TrustedStep::Host(host) => host.repo().to_owned(), _ => String::new() };
    let mut ctx = HostCtx::new(turn.token.clone(), std::sync::Arc::default(), ledger.push_trust_recheck(&proposal, &repo));
    ctx.timeout = codex_step::STEP_TIMEOUT;
    let cancel = ctx.cancel.clone();
    running.guards.push(CancelOnDrop(cancel.clone()));
    let root = PathBuf::from(&proposal.canonical_directory);
    let mut reply = call.reply;
    tokio::spawn(async move {
        let _slot = slot;
        let mut job = tokio::task::spawn_blocking(move || codex_step::run(step, &root, &ctx));
        let result = tokio::select! {
            result = &mut job => result.unwrap_or_else(|e| Err(e.to_string())),
            // The call was given up (its native turn ended, or the MCP wait
            // expired): the step is stopped, its slot freed once it is.
            () = reply.closed() => {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                // Stopped or not, what it held settles with what actually happened (GPT review).
                let ran = job.await.is_ok_and(|result| result.is_ok());
                if let Ok(mut finished) = outcomes.lock() {
                    finished.push((key, ran));
                }
                return;
            }
        };
        // Whatever the gate held under this step settles with its outcome.
        if let Ok(mut finished) = outcomes.lock() {
            finished.push((key, result.is_ok()));
        }
        let answer = match result {
            Ok(out) => (bounded(out), false),
            Err(why) => (bounded(format!("Hagency could not run this step: {why}")), true),
        };
        // The call may have gone just now: nobody to tell.
        let _ = reply.send(answer);
    });
}

/// The merge gate's answer to a review verdict sent as this step: the
/// pinned review to run, held under `key`, or `None` to leave the step to
/// push trust and the normal rules (GPT review of 2a7b1ed9): only a verdict
/// `ReviewPost` reads ever reaches the gate here (never a merge or ready),
/// a room member's needs the room's own review grant (its repository set
/// included) as well, and a gate refusal falls back to push trust.
async fn gated_review(ledger: &mut Ledger, turn: &Turn<'_>, proposal: &ToolProposal, command: &str, key: &str, source: &impl crate::merge_policy::github::PrSource) -> Option<TrustedStep> {
    let post = crate::watch_trust::review_post::ReviewPost::parse(command)?;
    if turn.scope.requester != turn.owner && !matches!(ledger.trusted_publish(turn.owner, proposal, super::now()), Ok(Some(TrustedStep::Command(_)))) {
        return None;
    }
    crate::merge_policy::gate::auto_answer(ledger, turn.owner, proposal, key, super::now(), source)
        .await?
        .then(|| TrustedStep::Command(post.command()))
}

/// The proposal push trust checks: the call as `codex_step::TOOL`, rooted
/// at the Room workspace, under the dispatch's current policy revisions.
fn proposal(ledger: &Ledger, turn: &Turn<'_>, command: &str) -> Option<ToolProposal> {
    let versions = ledger.policy_snapshot(turn.scope).ok()?;
    let proposal = ToolProposal {
        scope: turn.scope.clone(),
        dispatch: turn.dispatch.into(),
        tool: codex_step::TOOL.into(),
        arguments: json!({"command": command}),
        canonical_directory: turn.workspace.into(),
        risk: "high".into(),
        policy_revision: versions.map(|v| v.revision),
        expires: super::now() + PROPOSAL_LIFETIME_SECS,
    };
    Some(crate::watch_trust::for_workspace(&proposal, std::path::Path::new(turn.workspace)))
}

/// `text` cut to [`MAX_OUTPUT_BYTES`] on a character boundary.
fn bounded(mut text: String) -> String {
    if text.len() > MAX_OUTPUT_BYTES {
        let mut end = MAX_OUTPUT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[output cut]");
    }
    text
}

#[cfg(test)]
#[path = "repo_steps_tests.rs"]
mod tests;
