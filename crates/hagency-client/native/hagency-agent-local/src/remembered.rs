//! "Always allow": owner approvals an Agent may reuse without asking again.
//!
//! A rule is the EXACT request the owner approved, never a pattern, scoped to
//! the Agent, its room binding, the tool and the canonical folder:
//! - shell tools keep the exact command as sent (string or argv). Wrappers are
//!   never unwrapped: `sh -c x` and `zsh -lc x` load different startup files,
//!   so they are different requests (GPT review, Nov 15);
//! - only allowlisted argument fields may be present; an unknown field makes
//!   the request one-use, so a new execution-affecting field fails closed;
//! - commands that can reach the network or write remotely (`network`) are
//!   never remembered: "pushes and network steps never become automatic";
//! - the folder must canonicalize to itself (no symlinks) when the rule is
//!   made and every time it is matched;
//! - rules expire after [`RULE_LIFETIME_SECS`], bounding the residual risk
//!   that the same text later runs changed scripts or build files. A rule
//!   dated in the future (clock rollback) does not match.
//!
//! Residual risks, not closed here: the folder is matched by canonical path,
//! not by inode, so it can be swapped between the check and the launch; and a
//! remembered command runs whatever its scripts, Makefile, git config/aliases
//! and `PATH` say at the time it runs.
//!
//! Codex permission expansions, file changes and MCP calls are always one-use.
//! Rules apply in the `remember` and `outside` modes (an Agent without a saved
//! mode remembers); `ask` keeps them but asks every time. A match is checked
//! inside `validate_tool`, so it never overrides Deny or a tool/folder rule.
use crate::{Error, Ledger, Result, ToolProposal};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;

mod network;

/// The stored mode that reuses the owner's "Always allow" rules.
pub const MODE_REMEMBER: &str = "remember";
/// How long a remembered approval lasts (30 days).
pub const RULE_LIFETIME_SECS: i64 = 30 * 24 * 60 * 60;

/// Rules are keyed by room binding too. The table that keyed rules by Agent
/// only, with unwrapped shell lines, is dropped: those rules fail closed.
const SCHEMA: &str = "DROP TABLE IF EXISTS remembered_requests; CREATE TABLE IF NOT EXISTS remembered_rules (agent TEXT NOT NULL, binding TEXT NOT NULL, room TEXT NOT NULL, tool TEXT NOT NULL, directory TEXT NOT NULL, request TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY(agent,binding,room,tool,directory,request)) STRICT";
const MODES: &str = "CREATE TABLE IF NOT EXISTS approval_modes (agent TEXT PRIMARY KEY, mode TEXT NOT NULL CHECK(mode IN ('ask','remember','outside'))) STRICT";
/// Argument fields `claude.Bash` may carry; `timeout`/`run_in_background` are keyed.
const BASH_FIELDS: [&str; 4] = ["command", "description", "timeout", "run_in_background"];
/// Codex command-approval fields that are ids or explanations, not execution.
const CODEX_FIELDS: [&str; 9] =
    ["threadId", "turnId", "itemId", "approvalId", "reason", "command", "cwd", "commandActions", "startedAtMs"];
/// Codex fields allowed only when null (non-null makes it an expansion anyway).
const CODEX_NULL_FIELDS: [&str; 5] =
    ["grantRoot", "additionalPermissions", "networkApprovalContext", "proposedExecpolicyAmendment", "availableDecisions"];

/// Create the rules table and widen the mode table's CHECK to `remember`
/// (SQLite cannot alter a CHECK, so an older table is rebuilt with its rows).
pub(crate) fn ensure_schema(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(SCHEMA)?;
    let sql: Option<String> = db
        .query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='approval_modes'", [], |r| r.get(0))
        .optional()?;
    match sql {
        Some(sql) if sql.contains("'remember'") => Ok(()),
        Some(_) => db.execute_batch(&format!(
            "ALTER TABLE approval_modes RENAME TO approval_modes_before_remember; {MODES}; \
             INSERT INTO approval_modes SELECT agent,mode FROM approval_modes_before_remember; DROP TABLE approval_modes_before_remember;"
        )),
        None => db.execute_batch(MODES),
    }
}

/// One remembered approval, as the owner's panel lists it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RememberedRequest {
    pub tool: String,
    pub directory: String,
    /// The canonical request key (JSON): what must match exactly.
    pub request: String,
    #[serde(default)]
    pub created: i64,
    /// The room the rule was made in; empty in a revoke means every room.
    #[serde(default)]
    pub room: String,
}

/// Why "Always allow" is not offered for a shell request that reaches the
/// network, so the card can say so; other one-use requests return false.
pub fn is_network_step(proposal: &ToolProposal) -> bool {
    matches!(proposal.tool.as_str(), "claude.Bash" | "codex.command")
        && proposal.arguments.get("command").is_some_and(command_reaches_network)
}

fn command_reaches_network(command: &Value) -> bool {
    if let Some(line) = command.as_str() {
        return network::line_reaches_network(line);
    }
    let argv: Option<Vec<&str>> = command.as_array().and_then(|a| a.iter().map(Value::as_str).collect());
    argv.is_none_or(|argv| network::argv_reaches_network(&argv))
}

/// The exact command, when it may be remembered: non-empty, local only.
fn exact_command(args: &Map<String, Value>) -> Option<Value> {
    let command = args.get("command")?;
    let non_empty = match command {
        Value::String(line) => !line.trim().is_empty(),
        Value::Array(words) => !words.is_empty() && words.iter().all(Value::is_string),
        _ => false,
    };
    (non_empty && !command_reaches_network(command)).then(|| command.clone())
}

/// The folder canonicalizes to exactly itself: absolute, existing, no symlink.
fn canonical_folder(dir: &str) -> bool {
    Path::new(dir).is_absolute() && std::fs::canonicalize(dir).is_ok_and(|real| real == Path::new(dir))
}

/// Sort object keys at every level, so equal arguments give equal text
/// whatever order the runtime sent them in.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<_, _> = map.iter().map(|(k, v)| (k.clone(), canonical(v))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// What "Always allow" would remember for this request; `None` when it can
/// only be approved once.
pub fn request_key(proposal: &ToolProposal) -> Option<String> {
    if proposal.risk == "permission_expansion" || !canonical_folder(&proposal.canonical_directory) {
        return None;
    }
    let args = proposal.arguments.as_object()?;
    let only = |allowed: &[&str], nulls: &[&str]| {
        args.iter().all(|(k, v)| allowed.contains(&k.as_str()) || (nulls.contains(&k.as_str()) && v.is_null()))
    };
    let key = match proposal.tool.as_str() {
        "claude.Bash" if only(&BASH_FIELDS, &[]) => {
            let mut key = Map::new();
            key.insert("command".into(), exact_command(args)?);
            for field in ["timeout", "run_in_background"] {
                if let Some(value) = args.get(field) {
                    key.insert(field.into(), value.clone());
                }
            }
            Value::Object(key)
        }
        "codex.command" if only(&CODEX_FIELDS, &CODEX_NULL_FIELDS) => {
            let cwd = args.get("cwd")?.as_str().filter(|cwd| *cwd == proposal.canonical_directory)?;
            serde_json::json!({"command": exact_command(args)?, "cwd": cwd})
        }
        "claude.Bash" | "codex.command" => return None,
        // Claude's other tools: the exact arguments (path, content, pattern).
        tool if tool.starts_with("claude.") => canonical(&proposal.arguments),
        _ => return None,
    };
    serde_json::to_string(&key).ok()
}

/// True when the owner remembered exactly this request for this Agent, room
/// and folder within the rule lifetime. `saved_mode` is the Agent's stored
/// mode; none saved means remember.
pub(crate) fn remembered(db: &Connection, proposal: &ToolProposal, saved_mode: Option<&str>, now: i64) -> Result<bool> {
    if saved_mode == Some(crate::outside::MODE_ASK) {
        return Ok(false);
    }
    let Some(request) = request_key(proposal) else { return Ok(false) };
    let s = &proposal.scope;
    Ok(db
        .query_row(
            "SELECT 1 FROM remembered_rules WHERE agent=? AND binding=? AND room=? AND tool=? AND directory=? AND request=? AND created>? AND created<=?",
            (&s.agent, &s.binding, &s.room, &proposal.tool, &proposal.canonical_directory, &request, now - RULE_LIFETIME_SECS, now),
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

impl Ledger {
    /// Record "Always allow" for exactly this request in its room and folder.
    pub fn remember_request(&mut self, owner: &str, proposal: &ToolProposal, now: i64) -> Result<RememberedRequest> {
        self.owner(owner)?;
        let request = request_key(proposal).ok_or(Error::Invalid("request cannot be remembered"))?;
        let s = &proposal.scope;
        let rule = RememberedRequest {
            tool: proposal.tool.clone(), directory: proposal.canonical_directory.clone(), request, created: now, room: s.room.clone(),
        };
        self.db.execute(
            "INSERT INTO remembered_rules VALUES (?,?,?,?,?,?,?) ON CONFLICT DO UPDATE SET created=excluded.created",
            (&s.agent, &s.binding, &s.room, &rule.tool, &rule.directory, &rule.request, now),
        )?;
        Ok(rule)
    }
    /// The Agent's remembered approvals in every room, oldest first.
    pub fn remembered_requests(&self, owner: &str, agent: &str) -> Result<Vec<RememberedRequest>> {
        self.owner(owner)?;
        let mut statement =
            self.db.prepare("SELECT tool,directory,request,created,room FROM remembered_rules WHERE agent=? ORDER BY created,tool,request,room")?;
        let rows = statement.query_map([agent], |r| {
            Ok(RememberedRequest { tool: r.get(0)?, directory: r.get(1)?, request: r.get(2)?, created: r.get(3)?, room: r.get(4)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
    /// Revoke one remembered approval (in every room when `rule.room` is
    /// empty); true when it existed.
    pub fn forget_request(&mut self, owner: &str, agent: &str, rule: &RememberedRequest) -> Result<bool> {
        self.owner(owner)?;
        Ok(self.db.execute(
            "DELETE FROM remembered_rules WHERE agent=? AND tool=? AND directory=? AND request=? AND (?='' OR room=?)",
            (agent, &rule.tool, &rule.directory, &rule.request, &rule.room, &rule.room),
        )? > 0)
    }
}

#[cfg(test)]
mod tests;
