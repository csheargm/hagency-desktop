//! The issue loop's give-up step under "push freely" (live Nov 18, #96: the
//! comment raised an owner card that expired, so the run counted as stopped
//! on a card). Exactly one shape runs without a card, the one the hand-off
//! states ([`crate::issue_track::cannot_fix_step`]):
//!
//! `gh issue comment <N> --repo <owner/repo> --body '<CANNOT_FIX_MARKER>...'`
//!
//! `<N>` an issue the Agent was handed on that watched repository whose
//! track is still open ([`crate::issue_track::is_handed`]); `-R`/`-b` are
//! the same options. The binding is to the issue, not to the hand-off's
//! run: any run push trust already lets publish (open a PR closing the
//! issue) may give it up (GPT review, Nov 18). The body is one single-quoted line opening with the
//! marker, capped in length, with no quote, `$`, backtick or control
//! character. No other option, no chaining, redirect or heredoc. What runs
//! is rebuilt from the checked words (each single-quoted, pinned to
//! github.com with git's helpers off), never the model's own text. Every
//! other comment, edit or close keeps asking.
use crate::issue_track::{CANNOT_FIX_MARKER, is_handed};
use crate::outside::shell;
use rusqlite::Connection;

/// A give-up reason is one short line; the tracker only reads the marker.
const MAX_BODY_CHARS: usize = 1000;

/// One cannot-fix comment, as checked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CannotFix<'a> {
    number: u64,
    repo: &'a str,
    body: &'a str,
}

impl<'a> CannotFix<'a> {
    /// Read `line` (its parsed `words`) as one cannot-fix comment.
    pub(crate) fn parse(line: &str, words: &[&'a str]) -> Option<Self> {
        let ["gh", "issue", "comment", number, rest @ ..] = words else { return None };
        // `2>&1` is the one redirect the reader drops silently: refuse it here.
        if line.contains("2>&1") {
            return None;
        }
        // 1..=i32::MAX, digits only: gh reads anything else as a URL or fails.
        let number_ok = number.bytes().all(|b| b.is_ascii_digit());
        let number = number.parse().ok().filter(|n| number_ok && (1..=i32::MAX as u64).contains(n))?;
        let (mut repo, mut body) = (None, None);
        let mut words = rest.iter();
        while let Some(name) = words.next() {
            let slot = match *name {
                "-R" | "--repo" => &mut repo,
                "-b" | "--body" => &mut body,
                _ => return None,
            };
            if slot.replace(*words.next()?).is_some() {
                return None;
            }
        }
        let body = body.filter(|b| body_ok(line, b))?;
        Some(Self { number, repo: crate::merge_policy::gate::repo_name(repo?)?, body })
    }

    /// The pinned command that runs in place of the model's text.
    pub(crate) fn command(&self) -> String {
        let q = super::quote;
        let line = format!("GH_HOST='github.com' gh issue comment {} --repo {} --body {}", self.number, q(self.repo), q(self.body));
        super::pin_env(&line)
    }
}

/// The marker first, one line within the cap, nothing a shell could read
/// as more than text, and single-quoted in `line` as the agent wrote it.
fn body_ok(line: &str, body: &str) -> bool {
    let inert = !body.chars().any(|c| c.is_control() || matches!(c, '\'' | '"' | '`' | '$' | '\\'));
    inert && body.starts_with(CANNOT_FIX_MARKER) && body.chars().count() <= MAX_BODY_CHARS && line.contains(&format!("'{body}'"))
}

/// How push trust answers `line` (one shell step) when it is the cannot-fix
/// comment on an issue handed to `agent` on one of `repos`, else `None`.
pub(crate) fn step(db: &Connection, agent: &str, line: &str, repos: &[String]) -> crate::Result<Option<super::TrustedStep>> {
    let Some(parsed) = shell::parse(line).filter(|p| p.heredoc_at.is_none()) else { return Ok(None) };
    let [words] = parsed.segments.as_slice() else { return Ok(None) };
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let Some(comment) = CannotFix::parse(line, &words) else { return Ok(None) };
    let Some(repo) = repos.iter().find(|r| r.eq_ignore_ascii_case(comment.repo)) else { return Ok(None) };
    Ok(is_handed(db, agent, repo, comment.number)?.then(|| super::TrustedStep::Command(comment.command())))
}

#[cfg(test)]
#[path = "cannot_fix_tests.rs"]
mod tests;
