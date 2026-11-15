//! "Always allow": owner approvals an Agent may reuse without asking again.
//!
//! Ported from the original demo (snapshot 1949906dd) with one deliberate
//! narrowing: a rule is the EXACT request the owner approved, never a pattern.
//! The snapshot remembered a command's first word ("always allow `git`"), which
//! would later have allowed `git push`; here a rule holds the tool, the
//! canonical folder and the request's canonical key (the exact command line
//! and `cwd` for shell tools, the exact arguments for Claude's other tools), so
//! only an identical request is allowed again. Codex permission expansions
//! (network, other folders), Codex file changes (their preview ids differ per
//! request) and MCP calls can only be approved once.
//!
//! Rules apply in the `remember` and `outside` modes (an Agent without a saved
//! mode remembers); `ask` keeps them but asks every time. A match is checked
//! inside `validate_tool`, so it never overrides Deny or a tool/folder rule.
use crate::{Error, Ledger, Result, ToolProposal};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The stored mode that reuses the owner's "Always allow" rules.
pub const MODE_REMEMBER: &str = "remember";

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS remembered_requests (agent TEXT NOT NULL, tool TEXT NOT NULL, directory TEXT NOT NULL, request TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY(agent,tool,directory,request)) STRICT";
const MODES: &str = "CREATE TABLE IF NOT EXISTS approval_modes (agent TEXT PRIMARY KEY, mode TEXT NOT NULL CHECK(mode IN ('ask','remember','outside'))) STRICT";

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
}

/// The shell command a request would run: Codex's `["bash","-lc","<line>"]`
/// and `/bin/zsh -lc "<line>"` wrappers are unwrapped only when the script is
/// exactly one argument, so the key is the line the shell really runs.
pub fn command_line(arguments: &Value) -> Option<String> {
    let command = arguments.get("command").or_else(|| arguments.get("cmd"))?;
    if let Some(text) = command.as_str() {
        return Some(unwrap_shell_string(text.trim()));
    }
    let parts: Vec<&str> = command.as_array()?.iter().map(Value::as_str).collect::<Option<_>>()?;
    if let [shell, "-lc" | "-c", line] = parts.as_slice()
        && is_shell(shell)
    {
        return Some((*line).trim().to_owned());
    }
    // Any other array keeps its exact words; JSON keeps their boundaries.
    serde_json::to_string(&parts).ok()
}

fn is_shell(word: &str) -> bool {
    matches!(word.rsplit('/').next(), Some("sh" | "bash" | "zsh" | "dash" | "ksh"))
}

fn unwrap_shell_string(text: &str) -> String {
    let mut words = text.splitn(3, char::is_whitespace);
    if let (Some(shell), Some("-c" | "-lc"), Some(rest)) = (words.next(), words.next(), words.next())
        && is_shell(shell)
    {
        let rest = rest.trim();
        for quote in ['"', '\''] {
            if let Some(inner) = rest.strip_prefix(quote).and_then(|r| r.strip_suffix(quote))
                && !inner.contains(quote)
                && !inner.contains('\\')
            {
                return inner.to_owned();
            }
        }
    }
    text.to_owned()
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
    if proposal.risk == "permission_expansion" {
        return None;
    }
    let args = &proposal.arguments;
    let key = match proposal.tool.as_str() {
        "claude.Bash" => serde_json::json!({"command": command_line(args).filter(|l| !l.is_empty())?}),
        "codex.command" => {
            let cwd = args.get("cwd").map(|v| v.as_str().map(str::to_owned));
            serde_json::json!({"command": command_line(args).filter(|l| !l.is_empty())?, "cwd": cwd?.unwrap_or_default()})
        }
        // Claude's other tools: the exact arguments (path, content, pattern).
        tool if tool.starts_with("claude.") && args.is_object() => canonical(args),
        _ => return None,
    };
    serde_json::to_string(&key).ok()
}

/// True when the owner remembered exactly this request for this Agent and folder.
/// `saved_mode` is the Agent's stored mode; none saved means remember.
pub(crate) fn remembered(db: &Connection, proposal: &ToolProposal, saved_mode: Option<&str>) -> Result<bool> {
    if saved_mode == Some(crate::outside::MODE_ASK) {
        return Ok(false);
    }
    let Some(request) = request_key(proposal) else { return Ok(false) };
    Ok(db
        .query_row(
            "SELECT 1 FROM remembered_requests WHERE agent=? AND tool=? AND directory=? AND request=?",
            (&proposal.scope.agent, &proposal.tool, &proposal.canonical_directory, &request),
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

impl Ledger {
    /// Record "Always allow" for exactly this request in its folder.
    pub fn remember_request(&mut self, owner: &str, proposal: &ToolProposal, now: i64) -> Result<RememberedRequest> {
        self.owner(owner)?;
        let request = request_key(proposal).ok_or(Error::Invalid("request cannot be remembered"))?;
        let rule = RememberedRequest { tool: proposal.tool.clone(), directory: proposal.canonical_directory.clone(), request, created: now };
        self.db.execute(
            "INSERT INTO remembered_requests VALUES (?,?,?,?,?) ON CONFLICT DO NOTHING",
            (&proposal.scope.agent, &rule.tool, &rule.directory, &rule.request, now),
        )?;
        Ok(rule)
    }
    /// The Agent's remembered approvals, oldest first.
    pub fn remembered_requests(&self, owner: &str, agent: &str) -> Result<Vec<RememberedRequest>> {
        self.owner(owner)?;
        let mut statement = self.db.prepare("SELECT tool,directory,request,created FROM remembered_requests WHERE agent=? ORDER BY created,tool,request")?;
        let rows = statement.query_map([agent], |r| Ok(RememberedRequest { tool: r.get(0)?, directory: r.get(1)?, request: r.get(2)?, created: r.get(3)? }))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
    /// Revoke one remembered approval; true when it existed.
    pub fn forget_request(&mut self, owner: &str, agent: &str, rule: &RememberedRequest) -> Result<bool> {
        self.owner(owner)?;
        Ok(self.db.execute(
            "DELETE FROM remembered_requests WHERE agent=? AND tool=? AND directory=? AND request=?",
            (agent, &rule.tool, &rule.directory, &rule.request),
        )? > 0)
    }
}

#[cfg(test)]
mod tests;
