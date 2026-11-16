//! The "Repositories and roles" text of the Work block (ADR 0016), one
//! repository per entry (`;` or a new line between entries):
//! `owner/name developer|reviewer|maintainer [all] [any-path] [max=N]
//! [authors=a,b] [reviewers=c] [human=glob,glob]`. Parsing is strict: an
//! unknown word refuses the whole list, so a typo never loosens a rule.
use ::hagency::issue_watch::{IssueFilter, OwnerRules, Role};
use serde_json::Value;

/// The shape shown under the input.
pub const WORK_HINT: &str = "One per line or separated by ;  owner/name developer|reviewer|maintainer, then optional: all (any issue author), any-path, max=N, authors=login,login (or *), reviewers=login, human=glob";

/// The entries of `text` for `room_id`; `previous` keeps each listed
/// repository's start time, new ones start at `now`.
pub fn parse(text: &str, room_id: &str, previous: &[OwnerRules], now: i64) -> Result<Vec<OwnerRules>, String> {
    text.split(['\n', ';']).map(str::trim).filter(|l| !l.is_empty()).map(|line| entry(line, room_id, previous, now)).collect()
}

fn entry(line: &str, room_id: &str, previous: &[OwnerRules], now: i64) -> Result<OwnerRules, String> {
    let mut words = line.split_whitespace();
    let repo = words.next().unwrap_or_default().to_owned();
    let role = match words.next() {
        Some("developer") => Role::Developer,
        Some("reviewer") => Role::Reviewer,
        Some("maintainer") => Role::Maintainer,
        other => return Err(format!("{repo}: the role must be developer, reviewer or maintainer, not {}", other.unwrap_or("nothing"))),
    };
    let since = previous.iter().find(|p| p.repo.eq_ignore_ascii_case(&repo)).map_or(now, |p| p.since);
    let mut rules = OwnerRules { repo, role, room_id: room_id.to_owned(), since, ..Default::default() };
    let list = |v: &str| v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect::<Vec<_>>();
    for word in words {
        match word.split_once('=') {
            None if word == "all" => rules.issues = IssueFilter::All,
            None if word == "any-path" => rules.any_path = true,
            Some(("max", n)) => rules.max_lines = Some(n.parse().map_err(|_| format!("{}: max must be a number", rules.repo))?),
            Some(("authors", v)) => rules.trusted_authors = list(v),
            Some(("reviewers", v)) => rules.trusted_reviewers = list(v),
            Some(("human", v)) => rules.always_human = list(v),
            _ => return Err(format!("{}: unknown option {word}", rules.repo)),
        }
    }
    rules.clone().validate().map_err(|e| format!("{}: {e}", rules.repo))
}

/// The saved list as text again (the input's contents after a read).
pub fn render(work: &[OwnerRules]) -> String {
    work.iter().map(|w| {
        let role = match w.role { Role::Developer => "developer", Role::Reviewer => "reviewer", Role::Maintainer => "maintainer" };
        let mut words = vec![w.repo.clone(), role.to_owned()];
        if w.issues == IssueFilter::All { words.push("all".into()); }
        if w.any_path { words.push("any-path".into()); }
        if let Some(max) = w.max_lines { words.push(format!("max={max}")); }
        for (key, list) in [("authors", &w.trusted_authors), ("reviewers", &w.trusted_reviewers), ("human", &w.always_human)] {
            if !list.is_empty() { words.push(format!("{key}={}", list.join(","))); }
        }
        words.join(" ")
    }).collect::<Vec<_>>().join("; ")
}

/// The saved work list of an owner-API reply.
pub fn saved(reply: &Value) -> Vec<OwnerRules> {
    serde_json::from_value(reply["work"].clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_repositories_with_different_roles_round_trip() {
        let text = "acme/app maintainer any-path authors=betty,carl reviewers=dora human=secrets/**; acme/docs developer all";
        let work = parse(text, "!r:x", &[], 100).unwrap();
        assert_eq!(work.len(), 2);
        assert_eq!((work[0].role, work[0].any_path, work[0].trusted_authors.len(), work[0].since), (Role::Maintainer, true, 2, 100));
        assert_eq!((work[1].role, work[1].issues), (Role::Developer, IssueFilter::All));
        assert_eq!(parse(&render(&work), "!r:x", &work, 999).unwrap(), work, "keeps each repository's start time");
    }

    #[test]
    fn a_typo_or_bad_value_refuses_the_whole_list() {
        for bad in ["acme/app maintainr", "acme/app maintainer anypath", "acme/app developer max=lots", "nope developer", "acme/app reviewer reviewers=*"] {
            assert!(parse(&format!("acme/ok developer; {bad}"), "!r:x", &[], 1).is_err(), "{bad}");
        }
        assert!(parse("", "!r:x", &[], 1).unwrap().is_empty(), "an empty list clears the work");
    }
}
