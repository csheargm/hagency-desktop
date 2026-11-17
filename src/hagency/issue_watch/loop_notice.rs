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
    /// An approval qualifies the head: `fixer` runs the pinned merge.
    Approved { target: Target, fixer: OwnedUserId },
    /// Approved and within the rules, but this device's GitHub login cannot merge.
    NeedsMaintainer { target: Target },
    /// The merge gate says a person must decide; `reason` is the gate's.
    NeedsOwner { target: Target, reason: String },
}

impl LoopNotice {
    fn target(&self) -> &Target {
        match self {
            Self::ReviewRequested { target, .. } | Self::Approved { target, .. } | Self::NeedsMaintainer { target } | Self::NeedsOwner { target, .. } => target,
        }
    }
    fn step(&self) -> &'static str {
        match self {
            Self::ReviewRequested { .. } => "review",
            _ => "merge",
        }
    }
    fn mention(&self) -> Option<&OwnedUserId> {
        match self {
            Self::ReviewRequested { reviewer, .. } => Some(reviewer),
            Self::Approved { fixer, .. } => Some(fixer),
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
                format!("🔎 Review requested — {reviewer} please review PR #{} in {} at head commit {}: {}\nTo review: {steps}", t.number, t.repo, t.head, t.url)
            }
            Self::Approved { fixer, .. } => format!(
                "✅ Approved at {short} — merging under the repo rules: {fixer} run exactly `gh pr merge {n} --squash --match-head-commit {head} --repo {repo}` as its own command. If GitHub refuses the merge, report why and do not retry.",
                n = t.number, head = t.head, repo = t.repo,
            ),
            Self::NeedsMaintainer { .. } => format!("✅ Approved at {short}, ready to merge — needs a maintainer: this owner's GitHub account cannot merge {}. PR #{}: {}", t.repo, t.number, t.url),
            Self::NeedsOwner { reason, .. } => format!("⚠️ PR #{} in {} at {short} needs the owner: {}. {}", t.number, t.repo, quoted_title(reason), t.url),
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
        let t = self.target();
        let key = blake3::hash(format!("{agent_id}\n{}\n{}\n{}", self.step(), t.url, t.head).as_bytes());
        OwnedTransactionId::from(format!("hagency-loop-{}", key.to_hex()))
    }
}
