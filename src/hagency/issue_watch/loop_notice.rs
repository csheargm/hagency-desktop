//! The review loop's owner-authored notices (ADR 0015): "🔎 Review
//! requested" for a reviewer Agent, "✅ Approved ... merging" for the fixer,
//! "needs a maintainer", "needs you" and a member-joined line. Each carries an
//! `io.hagency.review_loop` field and a transaction id fixed by its pull
//! request and head, so a retry is never posted twice. No pull request title,
//! body or branch name is ever copied in: only numbers, the repository, the
//! commit SHA and the policy's own reason, quoted as data.
use super::notice::quoted_title;
use matrix_sdk::ruma::events::Mentions;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::{OwnedTransactionId, OwnedUserId};
use serde_json::{Map, Value, json};

/// The content field that marks a message as the review loop's.
pub const MARKER: &str = "io.hagency.review_loop";

/// The once-per-head step of the "still a draft" notice: kept once per head
/// like the holds, and given back like a merge when its dispatch never ran.
pub const DRAFT_STEP: &str = ::hagency::issue_watch::READY_STEP;

/// An Agent as the room reads it: its display name, then its Matrix ID
/// (the mention it needs); the ID alone when it has no name.
fn named(name: &str, agent: &OwnedUserId) -> String {
    if name.is_empty() { agent.to_string() } else { format!("{name} ({agent})") }
}

/// One pull request head the loop acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub head: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopNotice {
    /// Ask `reviewer` (an Agent in the room) to review `Target`.
    ReviewRequested { target: Target, reviewer: OwnedUserId },
    /// An approval qualifies the head: `fixer` (display name `name`) runs the pinned merge.
    Approved { target: Target, fixer: OwnedUserId, name: String },
    /// The loop's pull request is still a draft after the fixer's run ended:
    /// `fixer` is asked once per head to mark it ready (step [`DRAFT_STEP`]).
    MarkReady { target: Target, fixer: OwnedUserId, name: String },
    /// Approved and within the rules, but this device's GitHub login cannot merge.
    /// `step` is a hold step, so the merge step stays free for this head.
    NeedsMaintainer { target: Target, step: String },
    /// The merge gate says a person must decide; `reason` is the gate's.
    /// `step` is the hold step of the reason's kind: when the reason clears
    /// (CI turns green on the same head), the merge is still told (integration GPT review).
    NeedsOwner { target: Target, reason: String, step: String },
    /// The issue loop's pull request cannot merge yet: each line names one
    /// missing thing and how to fix it; `step` keys the set (one notice each).
    Held { target: Target, step: String, missing: Vec<String> },
    /// The merge step could not reach GitHub several checks in a row.
    Unreachable { target: Target, step: String },
    /// A `review` or `merge` step's dispatch ended without the Agent running
    /// (`why` is the ledger's word); `step` keys the round's one line.
    NotStarted { target: Target, what: String, why: String, gave_up: bool, step: String },
}

impl LoopNotice {
    fn target(&self) -> &Target {
        match self {
            Self::ReviewRequested { target, .. } | Self::Approved { target, .. } | Self::NeedsMaintainer { target, .. } | Self::NeedsOwner { target, .. } | Self::Held { target, .. } | Self::Unreachable { target, .. } | Self::NotStarted { target, .. } | Self::MarkReady { target, .. } => target,
        }
    }
    /// The once-per-head loop step this notice is recorded under.
    pub(crate) fn step(&self) -> &str {
        match self {
            Self::ReviewRequested { .. } => "review",
            Self::MarkReady { .. } => DRAFT_STEP,
            Self::Held { step, .. } | Self::Unreachable { step, .. } | Self::NeedsOwner { step, .. } | Self::NeedsMaintainer { step, .. } | Self::NotStarted { step, .. } => step,
            _ => "merge",
        }
    }
    /// The Agent the notice addresses (and dispatches), if any.
    pub(crate) fn mention(&self) -> Option<&OwnedUserId> {
        match self {
            Self::ReviewRequested { reviewer, .. } => Some(reviewer),
            Self::Approved { fixer, .. } | Self::MarkReady { fixer, .. } => Some(fixer),
            _ => None,
        }
    }

    /// The plain text the Agents read. English on purpose: it is an instruction.
    pub fn body(&self) -> String {
        let t = self.target();
        let short = &t.head[..t.head.len().min(12)];
        match self {
            Self::ReviewRequested { reviewer, .. } => {
                let steps = ::hagency::REVIEW_STEPS.replace("<N>", &t.number.to_string()).replace("<owner/repo>", &t.repo);
                format!("{}{reviewer} {}: {}\nTo review: {steps}", ::hagency::issue_watch::REVIEW_NOTICE_LEAD, ::hagency::issue_watch::review_ask(t.number, &t.repo, &t.head), t.url)
            }
            Self::Approved { fixer, name, .. } => format!(
                "{lead}{short} — merging under the room rules: {who} run exactly `{command}` as its own command. If GitHub refuses the merge, report why and do not retry.",
                lead = ::hagency::issue_watch::MERGE_NOTICE_LEAD,
                who = named(name, fixer),
                command = ::hagency::issue_watch::merge_command(t.number, &t.head, &t.repo),
            ),
            // The full head pins the request (round-2 GPT review): a request run late, after a newer push, must not ready that push.
            Self::MarkReady { fixer, name, .. } => format!(
                "{lead}{who} {ask}: no review can start while it is a draft. If the fix is done and the pull request's head is still {short}, run exactly `gh pr ready {n} --repo {repo}` as its own command; if its head moved since, do not mark it ready and say so. Otherwise say what is left. {url}",
                lead = ::hagency::issue_watch::READY_NOTICE_LEAD,
                who = named(name, fixer),
                ask = ::hagency::issue_watch::ready_ask(t.number, &t.repo, &t.head),
                n = t.number,
                repo = t.repo,
                url = t.url,
            ),
            Self::NeedsMaintainer { .. } => format!("✅ Approved at {short}, ready to merge — needs someone with GitHub write access: this owner's GitHub account cannot merge {}. PR #{}: {}", t.repo, t.number, t.url),
            Self::NeedsOwner { reason, .. } => format!("⚠️ PR #{} in {} at {short} needs the owner: {}. {}", t.number, t.repo, quoted_title(reason), t.url),
            Self::Held { missing, .. } => {
                let lines: String = missing.iter().map(|line| format!("\n• {line}")).collect();
                format!("⚠️ PR #{} in {} at {short} cannot be merged yet. Missing:{lines}\n{}", t.number, t.repo, t.url)
            }
            Self::Unreachable { .. } => format!("⚠️ Can't reach GitHub to merge PR #{} in {} — retrying on every check. {}", t.number, t.repo, t.url),
            Self::NotStarted { what, why, gave_up, .. } => super::step_resend::not_started_line(what, why, *gave_up, t),
        }
    }

    pub fn content(&self) -> RoomMessageEventContent {
        let content = RoomMessageEventContent::text_plain(self.body());
        match self.mention() {
            Some(user) => content.add_mentions(Mentions::with_user_ids([user.clone()])),
            None => content,
        }
    }

    pub fn marker(&self) -> Map<String, Value> {
        let t = self.target();
        let mut extra = Map::new();
        extra.insert(MARKER.into(), json!({"step": self.step(), "repo": t.repo, "number": t.number, "head": t.head}));
        extra
    }

    /// Fixed by Agent, step, pull request and head: one notice per head.
    pub fn txn_id(&self, agent_id: &str) -> OwnedTransactionId {
        self.txn_id_again(agent_id, 0)
    }

    /// The `again`-th re-send of this notice is a new message (0: the first post).
    pub fn txn_id_again(&self, agent_id: &str, again: u32) -> OwnedTransactionId {
        let t = self.target();
        let again = if again == 0 { String::new() } else { format!("\nagain-{again}") };
        let key = blake3::hash(format!("{agent_id}\n{}\n{}\n{}{again}", self.step(), t.url, t.head).as_bytes());
        OwnedTransactionId::from(format!("hagency-loop-{}", key.to_hex()))
    }
}
