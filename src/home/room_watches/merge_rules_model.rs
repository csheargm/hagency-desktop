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

/// The editor's fields for `read`: the fields the gate judges by, so in the
/// trusted tier its own paths (empty: any path) and line limit (empty: none).
pub fn draft_of(read: &RoomRead) -> Draft {
    let policy = base(read);
    let max_lines = if policy.trusted { policy.trusted_max_lines.map(|n| n.to_string()).unwrap_or_default() } else { policy.max_changed_lines.to_string() };
    Draft {
        paths: tier_paths(&policy).join(", "),
        max_lines,
        approvals: policy.required_agent_approvals.max(MIN_AGENT_APPROVALS).to_string(),
        require_ci: policy.require_ci,
    }
}

/// The paths the gate judges `policy`'s tier by.
fn tier_paths(policy: &MergePolicy) -> &Vec<String> {
    if policy.trusted { &policy.trusted_paths } else { &policy.low_risk_paths }
}

fn line_limit(text: &str) -> Result<u64, String> {
    text.trim().parse::<u64>().ok().filter(|n| *n > 0).ok_or_else(|| tr("The line limit is a whole number above 0").to_owned())
}

/// The rules `draft` saves over `read` (its other keys, such as always-human
/// paths, are kept), editing the fields of `read`'s tier; the reason when a
/// field is not usable or the rules would be too long to store.
pub fn policy_of(draft: &Draft, read: &RoomRead) -> Result<MergePolicy, String> {
    // An untouched field keeps the room's own list: re-splitting a glob that holds a
    // comma would widen it (GPT review); kept whole, the render below refuses it.
    let paths: Vec<String> = if draft.paths == draft_of(read).paths { tier_paths(&base(read)).clone() } else {
        draft.paths.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned).collect()
    };
    let required_agent_approvals = draft.approvals.trim().parse::<u32>().ok().filter(|n| *n >= MIN_AGENT_APPROVALS)
        .ok_or_else(|| format("Approvals are a whole number, at least {n}", &[("n", MIN_AGENT_APPROVALS.to_string())]))?;
    let mut policy = MergePolicy { required_agent_approvals, require_ci: draft.require_ci, ..base(read) };
    if policy.trusted {
        if !draft.require_ci {
            return Err(tr("The trusted tier always requires CI").to_owned());
        }
        policy.trusted_paths = paths;
        policy.trusted_max_lines = if draft.max_lines.trim().is_empty() { None } else { Some(line_limit(&draft.max_lines)?) };
    } else {
        if paths.is_empty() {
            return Err(tr("Enter at least one low-risk path").to_owned());
        }
        policy.low_risk_paths = paths;
        policy.max_changed_lines = line_limit(&draft.max_lines)?;
    }
    crate::hagency::issue_watch::room_rules::rules_text(&policy).map(|_| policy)
}

/// Whether a save may go ahead over `now`, the rules read again just before it: not when this
/// account may no longer change them, or when they changed since the editor
/// was filled from `opened` (another admin saved), so a stale editor never
/// erases newer rules (GPT integration review). Matrix has no conditional
/// state write: a save landing between this read and the write still wins.
pub fn unchanged_since(opened: &RoomRead, now: &RoomRead, editable: bool) -> Result<(), String> {
    if !editable {
        return Err(tr("Only a room admin can change the merge rules.").to_owned());
    }
    if opened != now {
        return Err(tr("The merge rules changed since you opened them. The editor now shows the newer rules: make your change again and save.").to_owned());
    }
    Ok(())
}

fn protected() -> String {
    PROTECTED_PREFIXES.iter().map(|p| (*p).to_owned()).chain([CODEOWNERS.to_owned()]).collect::<Vec<_>>().join(", ")
}

/// The trusted tier's own paths and line limit, as the gate applies them.
fn trusted_scope(policy: &MergePolicy) -> (String, String) {
    let scope = if policy.trusted_paths.is_empty() { tr("any path (trusted tier)").to_owned() } else {
        format("{paths} (trusted tier)", &[("paths", policy.trusted_paths.join(", "))])
    };
    let limit = match policy.trusted_max_lines {
        Some(n) => format("Up to {n} changed lines", &[("n", n.to_string())]),
        None => tr("No line limit (trusted tier)").to_owned(),
    };
    (scope, limit)
}

fn rule_lines(policy: &MergePolicy) -> Vec<String> {
    let mut always = protected();
    if !policy.always_human.is_empty() {
        always = format!("{always}, {}", policy.always_human.join(", "));
    }
    let (scope, limit) = if policy.trusted { trusted_scope(policy) } else {
        (policy.low_risk_paths.join(", "), format("Up to {n} changed lines", &[("n", policy.max_changed_lines.to_string())]))
    };
    vec![
        format("Low-risk paths: {paths}", &[("paths", scope)]),
        limit,
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
        Shown::Ready { editable: true, read } if base(read).trusted => format!("{} {}",
            tr("You are a room admin: you can change these rules. Agents and other members cannot."),
            tr("These rules use the trusted tier: the paths and line limit below are the trusted tier's own; leave them empty for any path and no line limit.")),
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
