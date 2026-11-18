//! Filing an issue on one of the room's own linked repositories (ADR 0030,
//! "the room's discussion"): `gh issue create --repo <owner/name> --title
//! '<title>' --body '<body>'` runs without a card when a joined member of the
//! room asked, the Agent may work in the room and the room is Assist or
//! Autonomous (not Off, not paused). Only the repositories the room links
//! (`io.hagency.room_repos`, recorded by the host per turn for a joined
//! requester, [`super::room_scope::linked`]) count. Exactly those three
//! options, each once, no inline `=` form, bounded lengths; what runs is
//! rebuilt from the checked words, each single-quoted, on github.com.
use crate::ToolProposal;
use rusqlite::Connection;

/// The longest title and body a card-free issue may carry.
pub const MAX_TITLE_CHARS: usize = 256;
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// A checked `gh issue create`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct IssueCreate<'a> {
    pub repo: &'a str,
    pub title: &'a str,
    pub body: &'a str,
}

impl<'a> IssueCreate<'a> {
    /// `gh issue create` words with exactly `--repo`, `--title` and `--body`.
    pub(crate) fn parse(words: &[&'a str]) -> Option<Self> {
        let ["gh", "issue", "create", rest @ ..] = words else { return None };
        let (mut repo, mut title, mut body) = (None, None, None);
        let mut words = rest.iter();
        while let Some(word) = words.next() {
            let slot = match *word {
                "--repo" | "-R" => &mut repo,
                "--title" | "-t" => &mut title,
                "--body" | "-b" => &mut body,
                _ => return None,
            };
            if slot.replace(*words.next()?).is_some() {
                return None; // an option given twice
            }
        }
        let (repo, title, body) = (crate::merge_policy::gate::repo_name(repo?)?, title?, body?);
        let title_ok = !title.trim().is_empty() && title.chars().count() <= MAX_TITLE_CHARS && !title.chars().any(char::is_control);
        let body_ok = body.len() <= MAX_BODY_BYTES && !body.contains('\0');
        (title_ok && body_ok).then_some(Self { repo, title, body })
    }

    /// The pinned line: the checked words, each quoted, on github.com.
    pub(crate) fn command(&self) -> String {
        let words = ["issue", "create", "--repo", self.repo, "--title", self.title, "--body", self.body];
        let quoted: Vec<String> = words.iter().map(|w| super::quote(w)).collect();
        super::pin_env(&format!("GH_HOST='github.com' gh {}", quoted.join(" ")))
    }
}

/// The pinned issue filing for `proposal`, when every condition holds.
pub(crate) fn step(db: &Connection, proposal: &ToolProposal) -> crate::Result<Option<super::TrustedStep>> {
    if proposal.risk == "permission_expansion" || !super::is_shell_step(proposal) {
        return Ok(None);
    }
    let Some(line) = proposal.arguments["command"].as_str() else { return Ok(None) };
    // Any shell syntax beyond plain quoted words refuses (no chaining, no substitution).
    let Some(parsed) = crate::outside::shell::parse(line).filter(|p| p.heredoc_at.is_none()) else { return Ok(None) };
    let [words] = parsed.segments.as_slice() else { return Ok(None) };
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let Some(issue) = IssueCreate::parse(&words) else { return Ok(None) };
    let (agent, room, requester) = (&proposal.scope.agent, &proposal.scope.room, &proposal.scope.requester);
    if crate::room_autonomy::loop_may_work_in(db, agent, room).is_err() {
        return Ok(None);
    }
    // Recorded only for the room's owner or a maintainer, and only the room's own linked repositories.
    let linked = super::room_scope::linked(room, requester);
    if !linked.iter().any(|r| r.eq_ignore_ascii_case(issue.repo)) {
        return Ok(None);
    }
    Ok(Some(super::TrustedStep::Command(issue.command())))
}

#[cfg(test)]
#[path = "issue_create_tests.rs"]
mod tests;
