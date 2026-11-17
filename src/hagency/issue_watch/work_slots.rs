//! The open-pull-request cap of one repository (`open_prs`), split out of
//! `work.rs`: what is open, plus what was handed off and is on its way.
use super::work::{Holder, Worker, claim};
use ::hagency::issue_watch::{MAX_PAGES, PAGE_SIZE, GITHUB_API, Issue, OwnerRules, fetch_open_pr_count, fetch_open_pulls};
use std::collections::HashSet;

/// The slots left under `open_prs`: minus the login's open pull requests
/// (GitHub's search, so one beyond the newest page counts) and minus every
/// issue handed off earlier that this Agent still holds a live claim on and
/// no open pull request of the login closes yet (a hand-off still on its way
/// keeps its slot across polls; an own PR beyond the newest page is
/// not read, so its claim may count twice: over, never under, the cap; a claim expires, so a stalled one frees it;
/// GPT review P1). Errors fail the pass.
pub(super) async fn open_slots(worker: &Worker<'_>, entry: &OwnerRules, login: &str, seen: &HashSet<i64>, issues: &[Issue]) -> Result<PrSlots, String> {
    let Some(limit) = entry.open_prs else { return Ok(PrSlots::Unlimited) };
    if listing_truncated(issues.len()) {
        // A pending hand-off may sit past the page cap: fail closed (GPT review).
        return Err(crate::i18n::tr("Too many open issues to enforce the open pull request limit").to_owned());
    }
    let open = fetch_open_pr_count(worker.client, GITHUB_API, &entry.repo, login, worker.token).await.map_err(|e| e.to_string())?;
    if open >= u64::from(limit) {
        return Ok(PrSlots::Left(0));
    }
    let pulls = fetch_open_pulls(worker.client, GITHUB_API, &entry.repo, worker.token).await.map_err(|e| e.to_string())?;
    let own: Vec<&str> = pulls.iter().filter(|p| p.author.eq_ignore_ascii_case(login)).map(|p| p.body.as_str()).collect();
    let mut used = open;
    for issue in issues.iter().filter(|i| !i.is_pr && seen.contains(&i.number) && !own.iter().any(|b| closes_issue(b, &entry.repo, i.number))) {
        if used >= u64::from(limit) {
            break; // full either way
        }
        if claim(worker, entry, issue.number, login, false).await? == Holder::Ours {
            used += 1;
        }
    }
    Ok(PrSlots::left(limit, used))
}

/// The open-pull-request slots one pass may still fill (`open_prs` minus
/// the ones already open), reserved one per hand-off.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PrSlots {
    Unlimited,
    Left(u64),
}
impl PrSlots {
    pub(crate) fn left(limit: u32, open: u64) -> Self {
        Self::Left(u64::from(limit).saturating_sub(open))
    }
    pub(crate) fn is_full(&self) -> bool {
        *self == Self::Left(0)
    }
    pub(crate) fn take(&mut self, handed: usize) {
        if let Self::Left(left) = self {
            *left = left.saturating_sub(handed as u64);
        }
    }
}

/// Whether pull request description `body` closes issue `number` of `repo`
/// with a GitHub closing keyword: "Closes #12", "fixes owner/name#12" or
/// "resolves https://github.com/owner/name/issues/12".
pub(crate) fn closes_issue(body: &str, repo: &str, number: i64) -> bool {
    let lower = body.to_ascii_lowercase();
    let repo = repo.to_ascii_lowercase();
    let refs = [format!("#{number}"), format!("{repo}#{number}"), format!("https://github.com/{repo}/issues/{number}")];
    CLOSING_WORDS.iter().flat_map(|word| refs.iter().map(move |r| format!("{word} {r}"))).any(|link| {
        lower.match_indices(&link).any(|(at, m)| {
            let before = lower[..at].chars().next_back().is_none_or(|c| !c.is_ascii_alphanumeric());
            let after = lower[at + m.len()..].chars().next().is_none_or(|c| !c.is_ascii_digit());
            before && after
        })
    })
}
/// Whether an issue listing of `count` items hit the engine's page cap.
pub(crate) fn listing_truncated(count: usize) -> bool {
    count >= (PAGE_SIZE * MAX_PAGES) as usize
}
const CLOSING_WORDS: [&str; 9] = ["close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved"];

#[cfg(test)]
#[path = "work_tests.rs"]
mod tests;
