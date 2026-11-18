//! The Status tab's "Merge rules" section without widgets (ADR 0029): the
//! room's rules in plain lines for every member, and for an account the
//! room's power levels let send them, an editor (paths, line limit,
//! approvals, CI) whose fields become rules through the gate's own parser.
use crate::hagency::issue_watch::room_rules::RoomRead;
use crate::i18n::{format, tr};
use ::hagency::issue_watch::safeguards::{CODEOWNERS, MIN_AGENT_APPROVALS, MergePolicy, PROTECTED_PREFIXES, RoomRulesState, suggested_policy};

/// What the section shows for one room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    Loading,
    Failed(String),
    /// The room's rules and whether this account may change them.
    Ready { read: RoomRead, editable: bool },
}

/// The editor's fields, as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    /// Low-risk paths, comma separated.
    pub paths: String,
    pub max_lines: String,
    pub approvals: String,
    pub require_ci: bool,
}

/// The rules the editor starts from: the room's own when valid, else the suggestion.
fn base(read: &RoomRead) -> MergePolicy {
    match read.state() {
        RoomRulesState::Read(policy) => policy,
        RoomRulesState::Absent | RoomRulesState::Unreadable => suggested_policy(),
    }
}

/// The editor's fields for `read`.
pub fn draft_of(read: &RoomRead) -> Draft {
    let policy = base(read);
    Draft {
        paths: policy.low_risk_paths.join(", "),
        max_lines: policy.max_changed_lines.to_string(),
        approvals: policy.required_agent_approvals.max(MIN_AGENT_APPROVALS).to_string(),
        require_ci: policy.require_ci,
    }
}

/// The rules `draft` saves over `read` (its other keys, such as always-human
/// paths, are kept); the reason when a field is not usable.
pub fn policy_of(draft: &Draft, read: &RoomRead) -> Result<MergePolicy, String> {
    let paths: Vec<String> = draft.paths.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned).collect();
    if paths.is_empty() {
        return Err(tr("Enter at least one low-risk path").to_owned());
    }
    let max_changed_lines = draft.max_lines.trim().parse::<u64>().ok().filter(|n| *n > 0).ok_or_else(|| tr("The line limit is a whole number above 0").to_owned())?;
    let required_agent_approvals = draft.approvals.trim().parse::<u32>().ok().filter(|n| *n >= MIN_AGENT_APPROVALS)
        .ok_or_else(|| format("Approvals are a whole number, at least {n}", &[("n", MIN_AGENT_APPROVALS.to_string())]))?;
    let policy = MergePolicy { low_risk_paths: paths, max_changed_lines, required_agent_approvals, require_ci: draft.require_ci, ..base(read) };
    ::hagency::issue_watch::safeguards::render_policy(&policy).map(|_| policy).ok_or_else(|| tr("A path holds a character the merge rules cannot store").to_owned())
}

fn protected() -> String {
    PROTECTED_PREFIXES.iter().map(|p| (*p).to_owned()).chain([CODEOWNERS.to_owned()]).collect::<Vec<_>>().join(", ")
}

fn rule_lines(policy: &MergePolicy) -> Vec<String> {
    let mut always = protected();
    if !policy.always_human.is_empty() {
        always = format!("{always}, {}", policy.always_human.join(", "));
    }
    let scope = if policy.trusted { tr("any path (trusted tier)").to_owned() } else { policy.low_risk_paths.join(", ") };
    vec![
        format("Low-risk paths: {paths}", &[("paths", scope)]),
        format("Up to {n} changed lines", &[("n", policy.max_changed_lines.to_string())]),
        format("{n} approval(s): another Agent's, or under an owner's maintainer rules the maintainer's review of an outside author's PR or a trusted GitHub reviewer's", &[("n", policy.required_agent_approvals.max(MIN_AGENT_APPROVALS).to_string())]),
        tr(if policy.require_ci { "CI required" } else { "CI not required" }).to_owned(),
        format("Always a person: {paths}", &[("paths", always)]),
    ]
}

/// The section's plain lines, the same for members and admins.
pub fn lines(shown: &Shown) -> Vec<String> {
    let read = match shown {
        Shown::Loading => return vec![tr("Loading the merge rules…").to_owned()],
        Shown::Failed(error) => return vec![format("The merge rules are unavailable: {error}", &[("error", error.clone())])],
        Shown::Ready { read, .. } => read,
    };
    match (read, read.state()) {
        (RoomRead::Refused(why), _) => vec![format("The room's merge rules are ignored ({why}): every merge needs a person.", &[("why", tr(why).to_owned())])],
        (RoomRead::None, _) => vec![tr("No merge rules in this room: every merge needs a person.").to_owned()],
        (RoomRead::Rules { sender, .. }, RoomRulesState::Read(policy)) => {
            let mut out = rule_lines(&policy);
            out.push(format("Set by {who}", &[("who", sender.clone())]));
            out
        }
        (RoomRead::Rules { .. }, _) => vec![tr("The room's merge rules are not valid: every merge needs a person.").to_owned()],
    }
}

/// The hint under the section: who may change the rules.
pub fn hint(shown: &Shown) -> String {
    match shown {
        Shown::Ready { editable: true, .. } => tr("You are a room admin: you can change these rules. Agents and other members cannot.").to_owned(),
        Shown::Ready { .. } => tr("Only a room admin can change the merge rules.").to_owned(),
        Shown::Loading | Shown::Failed(_) => String::new(),
    }
}

/// Whether the editor is offered: only to an account that may send the rules.
pub fn editor_shown(shown: &Shown) -> bool {
    matches!(shown, Shown::Ready { editable: true, .. })
}

#[cfg(test)]
#[path = "merge_rules_model_tests.rs"]
mod tests;
