//! The loop notices' exact words, which the ledger reads back to tag each
//! received notice with the step it dispatches (split from `step_resend.rs`, R4).
use super::READY_STEP;
use crate::merge_policy::github::commit_sha;
use crate::Result;

/// How a merge notice starts; only a message that starts so is a loop notice.
pub const MERGE_NOTICE_LEAD: &str = "✅ Approved at ";
/// How a review request notice starts.
pub const REVIEW_NOTICE_LEAD: &str = "🔎 Review requested — ";

/// The exact merge command a "✅ Approved" notice tells the merger to run.
pub fn merge_command(number: u64, head: &str, repo: &str) -> String {
    format!("gh pr merge {number} --squash --match-head-commit {head} --repo {repo}")
}
/// The part of a "🔎 Review requested" notice naming the pull request and head.
pub fn review_ask(number: u64, repo: &str, head: &str) -> String {
    format!("please review PR #{number} in {repo} at head commit {head}")
}
/// How a "still a draft" notice starts.
pub const READY_NOTICE_LEAD: &str = "📝 Still a draft — ";
/// The part of a "still a draft" notice naming the pull request and the head it may be readied at.
pub fn ready_ask(number: u64, repo: &str, head: &str) -> String {
    format!("mark ready PR #{number} in {repo} at head commit {head}")
}

/// What one loop notice dispatches: step, pull request URL, head, and the
/// Matrix ID it addresses.
pub type LoopStep = (&'static str, String, String, String);

/// The step a loop notice's `body` dispatches: only an exact
/// [`merge_command`], [`review_ask`] or [`ready_ask`], addressed to the Matrix ID just before it.
pub fn loop_step_of(body: &str) -> Option<LoopStep> {
    // The ID may follow the Agent's display name in parentheses: "bird-claude (@agent:x)".
    let addressee = |lead: &str| body[..body.find(lead)?].split_whitespace().rev().map(|w| w.trim_start_matches('(').trim_end_matches(')')).find(|w| w.starts_with('@')).map(str::to_owned);
    let parse = |lead: &str, words: &[&str]| -> Option<(u64, String, String)> {
        let rest = &body[body.find(lead)? + lead.len()..];
        let mut it = rest.split_whitespace();
        let number: u64 = it.next()?.trim_start_matches('#').parse().ok().filter(|n| *n > 0)?;
        let mut out = Vec::new();
        for word in words {
            if word.is_empty() { out.push(it.next()?.trim_end_matches(['`', ':'])) } else if it.next()? != *word { return None }
        }
        let (head, repo) = if lead.starts_with("gh") { (out[0], out[1]) } else { (out[1], out[0]) };
        let ok = commit_sha(head).is_some() && crate::issue_watch::valid_repo(repo);
        ok.then(|| (number, repo.to_owned(), head.to_owned()))
    };
    let pick = |step: &'static str, (n, repo, head): (u64, String, String), (phrase, lead): (&str, &str)| {
        let exact = match step {
            "merge" => merge_command(n, &head, &repo),
            READY_STEP => ready_ask(n, &repo, &head),
            _ => review_ask(n, &repo, &head),
        };
        let to = addressee(phrase)?;
        // A message quoting a notice is no notice (GPT review): a line must start as one,
        // as a hand-off's lead may (`activity::handoff_issue`).
        let notice = body.lines().any(|line| line.trim_start().starts_with(lead));
        (notice && body.contains(&exact)).then(|| (step, crate::merge_policy::pending::pull_url(&repo, n), head, to))
    };
    const ASK: [&str; 6] = ["in", "", "at", "head", "commit", ""];
    if let Some(found) = parse("gh pr merge ", &["--squash", "--match-head-commit", "", "--repo", ""]) {
        return pick("merge", found, ("gh pr merge ", MERGE_NOTICE_LEAD));
    }
    if let Some(found) = parse("mark ready PR ", &ASK) {
        return pick(READY_STEP, found, ("mark ready PR ", READY_NOTICE_LEAD));
    }
    pick("review", parse("please review PR ", &ASK)?, ("please review PR ", REVIEW_NOTICE_LEAD))
}

/// Tag a just-received request `dispatch` with the loop step its `body`
/// dispatches, if any; tags of requests no longer in the inbox are dropped.
pub(crate) fn remember_loop_step(db: &rusqlite::Connection, dispatch: &str, body: &str) -> Result<()> {
    if let Some((step, pr, head, to)) = loop_step_of(body) {
        db.execute("INSERT OR IGNORE INTO loop_step_dispatches VALUES (?,?,?,?,?)", (dispatch, step, pr, head, to))?;
    }
    db.execute("DELETE FROM loop_step_dispatches WHERE dispatch NOT IN (SELECT id FROM local_inbox)", [])?;
    Ok(())
}

/// Whether request `dispatch` is the owner's loop asking `addressee` to
/// review exactly `pr` at `head` (a received "🔎 Review requested" notice).
pub(crate) fn asked_to_review(db: &rusqlite::Connection, dispatch: &str, (pr, head): (&str, &str), addressee: &str) -> Result<bool> {
    const ASKED: &str = "SELECT EXISTS(SELECT 1 FROM loop_step_dispatches WHERE dispatch=? AND step='review' AND lower(pr)=lower(?) AND head=? AND addressee=?)";
    Ok(db.query_row(ASKED, (dispatch, pr, head, addressee), |r| r.get(0))?)
}
