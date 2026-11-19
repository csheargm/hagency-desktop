//! Room merge rules (ADR 0029): the agent merge rules are Hagency policy of
//! the project room, not repository content. A room admin sets them as the
//! room state event [`MERGE_RULES_STATE_TYPE`] (state key ""), whose
//! `policy` field holds the same text [`super::parse_policy`] reads; the
//! homeserver lets only members at the event's power level (admins, by the
//! default state level) send it.
//!
//! The gate never reads Matrix. The owner's desktop reads the state from the
//! homeserver itself and records a snapshot per repository and room through
//! the owner API ([`Ledger::set_room_merge_rules`]): event id, sender, text
//! and when it was read. No Agent tool writes here. A merge is judged by the
//! snapshot of the merge's own room, only when a watch links the repository
//! to that room, and only while it is younger than [`ROOM_RULES_TTL_SECS`]; no watch,
//! no snapshot, a stale one or one that does not parse all need a person.
use super::{parse_policy, MergePolicy};
use crate::issue_watch::{valid_repo, valid_room_id};
use crate::{Error, Ledger, Result};
use serde::Deserialize;
use serde_json::{json, Value};

/// The room state event type that holds a room's merge rules.
pub const MERGE_RULES_STATE_TYPE: &str = "io.hagency.merge_rules";
/// The content field holding the rules text.
pub const POLICY_FIELD: &str = "policy";
/// How long a snapshot counts: the desktop re-reads every minute while it runs.
pub const ROOM_RULES_TTL_SECS: i64 = 300;
/// The longest rules text accepted (the desktop editor refuses longer rules before saving).
pub const MAX_POLICY_BYTES: usize = 8 * 1024;
/// What oversized rules are stored as: text [`parse_policy`] refuses.
const OVERSIZED: &str = "oversized: true";
/// What a snapshot the desktop could not read is stored as: it replaces the
/// last rules, so an admin's tightening is never merged past (GPT review).
const UNREAD: &str = "unread: true";
/// The gate's reason (and the room notice) when a room has no rules.
pub const NO_ROOM_RULES: &str = "no merge rules in this room — a room admin can set them in Room info → Status";
pub const STALE_ROOM_RULES: &str = "the room's merge rules were not re-read in the last 5 minutes (the owner's Hagency desktop re-reads them while it runs)";
pub const INVALID_ROOM_RULES: &str = "the room's merge rules are not valid — a room admin can fix them in Room info → Status";
pub const UNREAD_ROOM_RULES: &str = "the owner's Hagency desktop could not read the room's merge rules on its last check, so every merge needs a person until it reads them again";
pub const NO_WATCH_ROOM: &str = "no watch links this repository to this room, so no room merge rules apply here";

/// The editor's suggested low-risk paths: Markdown, docs, plain-text files
/// and the extension-less doc files at the repository root (owner, Nov 18).
pub const SUGGESTED_LOW_RISK_PATHS: [&str; 8] = ["**/*.md", "docs/**", "*.txt", "AUTHORS", "CONTRIBUTORS", "MAINTAINERS", "CHANGELOG", "NOTICE"];
/// Legal files and dependency lists the suggested paths would otherwise reach.
pub const SUGGESTED_ALWAYS_HUMAN: [&str; 3] = ["**/LICENSE*", "**/COPYING*", "**/requirements*.txt"];

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS room_merge_rules (repo TEXT NOT NULL, room TEXT NOT NULL, event_id TEXT NOT NULL, sender TEXT NOT NULL, policy TEXT, read_at INTEGER NOT NULL, PRIMARY KEY(repo,room)) STRICT;";

/// What the desktop read from the homeserver for one repository's room;
/// `policy: None` means the room has no (accepted) rules event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoomRulesSnapshot {
    pub repo: String,
    pub room_id: String,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub sender: Option<String>,
    #[serde(default)]
    pub policy: Option<String>,
    /// The desktop could not read the room's rules: the last ones stop counting.
    #[serde(default)]
    pub unread: bool,
}

/// The rules a merge is judged by, or why there are none (a person merges).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomRules {
    Rules(MergePolicy),
    Missing(&'static str),
}

impl RoomRules {
    /// The rules, when the room has usable ones.
    pub fn policy(&self) -> Option<&MergePolicy> {
        match self {
            Self::Rules(policy) => Some(policy),
            Self::Missing(_) => None,
        }
    }
}

/// The rules text of a merge-rules event's content.
pub fn policy_text(content: &Value) -> Option<&str> {
    content[POLICY_FIELD].as_str()
}
/// The parsed rules of a merge-rules event's content (the gate's own parser).
pub fn rules_of(content: &Value) -> Option<MergePolicy> {
    parse_policy(policy_text(content)?)
}
/// The event content for rules text.
pub fn content_of(text: &str) -> Value {
    json!({ POLICY_FIELD: text })
}

/// The suggested rules a room admin starts from: one Agent approval, no CI
/// requirement, up to 50 changed lines in [`SUGGESTED_LOW_RISK_PATHS`].
pub fn suggested_policy() -> MergePolicy {
    let mut policy = parse_policy("require_ci: false").expect("the suggestion parses");
    policy.low_risk_paths = SUGGESTED_LOW_RISK_PATHS.iter().map(|p| (*p).to_owned()).collect();
    policy.always_human = SUGGESTED_ALWAYS_HUMAN.iter().map(|p| (*p).to_owned()).collect();
    policy
}

/// A glob the policy text can carry inline: no quote, separator, comment,
/// bracket, control character or outer space.
fn plain_glob(glob: &str) -> bool {
    !glob.is_empty() && glob.trim() == glob && !glob.contains(['"', '\'', ',', '#', '[', ']']) && !glob.chars().any(char::is_control)
}

fn list_line(key: &str, items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|i| format!("\"{i}\"")).collect();
    format!("{key}: [{}]\n", quoted.join(", "))
}

/// `policy` as the text [`parse_policy`] reads back to the same rules;
/// `None` when a glob cannot be written safely (it is refused, not escaped).
pub fn render_policy(policy: &MergePolicy) -> Option<String> {
    let lists = [&policy.low_risk_paths, &policy.trusted_paths, &policy.always_human];
    if !lists.iter().all(|list| list.iter().all(|g| plain_glob(g))) {
        return None;
    }
    let mut text = list_line("low_risk_paths", &policy.low_risk_paths);
    text += &format!("max_changed_lines: {}\nrequired_agent_approvals: {}\nrequire_ci: {}\n", policy.max_changed_lines, policy.required_agent_approvals, policy.require_ci);
    if !policy.always_human.is_empty() {
        text += &list_line("always_human", &policy.always_human);
    }
    if policy.trusted {
        text += "tier: trusted\n";
        if !policy.trusted_paths.is_empty() {
            text += &list_line("paths", &policy.trusted_paths);
        }
        text += &policy.trusted_max_lines.map(|n| format!("max_lines: {n}\n")).unwrap_or_default();
    }
    let expected = MergePolicy { ci_if_present: false, ..policy.clone() };
    (parse_policy(&text).as_ref() == Some(&expected)).then_some(text)
}

pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn check(snapshot: &RoomRulesSnapshot) -> Result<()> {
    if !valid_repo(&snapshot.repo) || !valid_room_id(&snapshot.room_id) {
        return Err(Error::Invalid("room rules need a repository owner/name and a room ID"));
    }
    if snapshot.unread && snapshot.policy.is_some() {
        return Err(Error::Invalid("unread room rules carry no rules"));
    }
    if snapshot.policy.is_none() {
        return Ok(());
    }
    let id_ok = snapshot.event_id.as_deref().is_some_and(|e| e.starts_with('$') && e.len() <= 255);
    let sender_ok = snapshot.sender.as_deref().is_some_and(|s| s.starts_with('@') && s.contains(':') && s.len() <= 255);
    if !id_ok || !sender_ok {
        return Err(Error::Invalid("room rules need their event ID and sender"));
    }
    Ok(())
}

impl Ledger {
    /// Record what the owner's desktop read from the homeserver for
    /// `snapshot.repo` in `snapshot.room_id` at `now` (owner API only).
    pub fn set_room_merge_rules(&mut self, owner: &str, snapshot: &RoomRulesSnapshot, now: i64) -> Result<()> {
        self.owner(owner)?;
        check(snapshot)?;
        let (event, sender) = (snapshot.event_id.as_deref().unwrap_or(""), snapshot.sender.as_deref().unwrap_or(""));
        let (event, sender) = if snapshot.policy.is_some() { (event, sender) } else { ("", "") };
        // Oversized rules replace the old ones as invalid, never leave them in force (GPT review).
        let policy = snapshot.policy.as_deref().map(|t| if t.len() > MAX_POLICY_BYTES { OVERSIZED } else { t });
        let policy = if snapshot.unread { Some(UNREAD) } else { policy };
        self.db.execute(
            "INSERT OR REPLACE INTO room_merge_rules VALUES (?,?,?,?,?,?)",
            (snapshot.repo.to_ascii_lowercase(), &snapshot.room_id, event, sender, policy, now),
        )?;
        Ok(())
    }
    /// The recorded snapshot for `repo` in `room`: `(event id, sender, text, read at)`.
    pub fn room_merge_rules_record(&self, repo: &str, room: &str) -> Result<Option<(String, String, Option<String>, i64)>> {
        use rusqlite::OptionalExtension;
        let row = self.db.query_row(
            "SELECT event_id, sender, policy, read_at FROM room_merge_rules WHERE repo=? AND room=?",
            (repo.to_ascii_lowercase(), room),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        );
        Ok(row.optional()?)
    }
    /// The rules of `room` for `repo` at `now`, or why none apply.
    pub fn room_merge_rules(&self, repo: &str, room: &str, now: i64) -> Result<RoomRules> {
        let Some((_, _, text, read_at)) = self.room_merge_rules_record(repo, room)? else { return Ok(RoomRules::Missing(NO_ROOM_RULES)) };
        if now.saturating_sub(read_at) > ROOM_RULES_TTL_SECS || read_at > now.saturating_add(ROOM_RULES_TTL_SECS) {
            return Ok(RoomRules::Missing(STALE_ROOM_RULES));
        }
        if text.as_deref() == Some(UNREAD) {
            return Ok(RoomRules::Missing(UNREAD_ROOM_RULES));
        }
        Ok(match text.as_deref().map(parse_policy) {
            None => RoomRules::Missing(NO_ROOM_RULES),
            Some(None) => RoomRules::Missing(INVALID_ROOM_RULES),
            Some(Some(policy)) => RoomRules::Rules(policy),
        })
    }
    /// Whether `repo` is linked to `room` on this device: an active watch
    /// (or work list entry) of `repo`, by any Agent here, reports to `room`.
    /// A paused watch links nothing (GPT review).
    pub fn watch_links(&self, repo: &str, room: &str) -> Result<bool> {
        for agent in self.watching_agents()? {
            if self.watch_entries(&agent)?.iter().any(|e| e.active && e.repo.eq_ignore_ascii_case(repo) && e.room_id == room) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// The room rules a merge (or review) of `repo` from `room` is judged by
    /// now: that room's own rules, and only when `repo` is linked to it, so
    /// the rules, the autonomy level, the role and the approvals all refer to
    /// the same room (batch GPT review: a merge borrowed another room's rules).
    pub(crate) fn rules_for_merge(&self, repo: &str, room: &str) -> Result<RoomRules> {
        if !self.watch_links(repo, room)? {
            return Ok(RoomRules::Missing(NO_WATCH_ROOM));
        }
        self.room_merge_rules(repo, room, unix_now())
    }
}

#[cfg(test)]
#[path = "room_rules_tests.rs"]
mod tests;
