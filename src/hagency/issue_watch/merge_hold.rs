//! The merge step of the issue loop's own pull request (ADR 0020): once its
//! review was asked, every check either carries an approved head on to the
//! pinned merge, waits (the reviewer has not answered, CI is running, the
//! facts moved), or posts ONE "cannot be merged yet" notice per head and set
//! of missing things, naming each one and how to fix it. The status line
//! says the same. The merge gate (`readiness`) stays the only judge.
use super::loop_notice::{LoopNotice, Target};
use super::notice::quoted_title;
use super::review_loop::{self, LoopCtx, LoopIo, can_merge};
use ::hagency::issue_watch::HOLD_STEP;
use serde_json::{Value, json};

/// How long after the pull request opened the loop waits for a verdict
/// before it says that no Agent approval counts.
pub const REVIEW_WAIT_SECS: i64 = 30 * 60;

/// One thing the pull request still needs before an Agent may merge it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Missing {
    /// No Agent approval of this head is recorded in this device's ledger.
    Approval,
    /// The policy requires CI and a check failed or none ran.
    Ci,
    /// The repository rules (paths, size, author) need a person.
    Rules,
    /// The merging Agent's "Pull request merges" switch is off.
    MergesOff,
    /// This owner's GitHub account cannot merge the repository.
    Permission,
}

impl Missing {
    /// The word in the notice's once-per-set step.
    fn word(self) -> &'static str {
        match self {
            Self::Approval => "approvals",
            Self::Ci => "ci",
            Self::Rules => "rules",
            Self::MergesOff => "merges-off",
            Self::Permission => "permission",
        }
    }
    /// The status line's part (UI text, translated by the caller).
    pub fn label(self) -> &'static str {
        match self {
            Self::Approval => "an Agent approval",
            Self::Ci => "green CI",
            Self::Rules => "a person (repo rules)",
            Self::MergesOff => "Pull request merges switched on",
            Self::Permission => "a maintainer to merge",
        }
    }
    /// The notice's line: what is missing and how to fix it. English on
    /// purpose (room content); the gate's reason is quoted as data.
    fn line(self, reason: &str, repo: &str) -> String {
        let said = quoted_title(reason);
        match self {
            Self::Approval => format!("an Agent approval of this head (the merge gate says {said}). Only an approval another Agent posts through Hagency's approval gate counts; a Codex Agent's review or a plain GitHub comment is not recorded. Ask another (Claude) Agent in this room to review it again, or review and merge it yourself."),
            Self::Ci => format!("green CI (the merge gate says {said}). Fix or re-run the checks, or merge it yourself."),
            Self::Rules => format!("a person: the repo rules say {said}. Review and merge it yourself."),
            Self::MergesOff => "Pull request merges switched on for this Agent: it is off, so no Agent may merge. Turn it on in the Agent's settings, or merge it yourself.".to_owned(),
            Self::Permission => format!("a maintainer: this owner's GitHub account cannot merge {repo}. Someone with write access must merge it."),
        }
    }
}

/// What the merge step does for one head this check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    /// No verdict yet from the reviewer (and not long enough to stop waiting).
    WaitReview,
    /// CI is still running under a policy that requires it.
    WaitCi,
    /// The gate's facts are unreadable or for another head: ask again next check.
    WaitFacts,
    /// Every rule passes and the owner allows Agent merges: merge.
    Merge,
    /// What is missing, each with the gate's reason (empty when it has none).
    Hold(Vec<(Missing, String)>),
}

/// Whether the reviewer has answered for `head` (any GitHub review of exactly
/// that commit: every Agent shares one login), or a pull request nobody
/// ever reviewed has waited [`REVIEW_WAIT_SECS`]. After a push to a reviewed
/// pull request the loop waits for the verdict on the new head.
pub fn review_settled(reviews: &[(String, String, String)], head: &str, opened_at: i64, now: i64) -> bool {
    reviews.iter().any(|(_, _, commit)| commit == head) || (reviews.is_empty() && now - opened_at >= REVIEW_WAIT_SECS)
}

/// The merge step for `target` from the gate's `readiness` answer, this
/// owner's GitHub `permission`, and whether the review has `settled`.
pub fn gate_of(target: &Target, readiness: &Value, permission: Option<&str>, settled: bool) -> Gate {
    if readiness["head"].as_str() != Some(target.head.as_str()) {
        return Gate::WaitFacts;
    }
    let mut missing = Vec::new();
    match readiness["decision"].as_str() {
        Some("merge") => {}
        Some("wait") => return Gate::WaitCi,
        _ => {
            let kind = match readiness["kind"].as_str() {
                Some("approvals") if !settled => return Gate::WaitReview,
                Some("approvals") => Missing::Approval,
                Some("ci") => Missing::Ci,
                _ => Missing::Rules,
            };
            missing.push((kind, readiness["reason"].as_str().unwrap_or("the merge could not be judged").to_owned()));
        }
    }
    // Only an explicit "off" holds: an older owner API that sends no switch
    // leaves it to the gate, which asks the owner's card when it is off.
    if readiness["autoMerge"] == false {
        missing.push((Missing::MergesOff, String::new()));
    }
    if !can_merge(permission) {
        missing.push((Missing::Permission, String::new()));
    }
    if missing.is_empty() { Gate::Merge } else { Gate::Hold(missing) }
}

/// The once-per-set step: `hold-` and the missing words in a fixed order.
fn hold_step(missing: &[(Missing, String)]) -> String {
    let mut words: Vec<Missing> = missing.iter().map(|(m, _)| *m).collect();
    words.sort();
    words.dedup();
    format!("{HOLD_STEP}{}", words.iter().map(|m| m.word()).collect::<Vec<_>>().join("-"))
}

/// Act on the merge step for `target`: the pinned merge for `ctx.agent`
/// (once per head, through the review loop's own merge step), or one notice
/// per head and set of missing things. Returns what the step found.
pub async fn follow_gate(io: &impl LoopIo, target: &Target, readiness: &Value, ctx: &LoopCtx<'_>, settled: bool) -> Result<Gate, String> {
    let gate = gate_of(target, readiness, ctx.permission, settled);
    match &gate {
        Gate::Merge => review_loop::advance_merge(io, target, ctx).await?,
        Gate::Hold(missing) => {
            let step = hold_step(missing);
            let key = json!({"step": step, "pr": target.url, "head": target.head});
            if io.review(json!({"stepTaken": key})).await?["taken"] != true {
                let lines = missing.iter().map(|(m, reason)| m.line(reason, &target.repo)).collect();
                let notice = LoopNotice::Held { target: target.clone(), step, missing: lines };
                review_loop::post_then_claim(io, &notice, key).await?;
            }
        }
        Gate::WaitReview | Gate::WaitCi | Gate::WaitFacts => {}
    }
    Ok(gate)
}
