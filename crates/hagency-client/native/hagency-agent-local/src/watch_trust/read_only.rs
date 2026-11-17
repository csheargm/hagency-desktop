//! Read-only GitHub queries on a repository the Agent WATCHES from this Room
//! run without a card (ADR 0019): `gh issue list|view`, `gh pr
//! list|view|diff|checks`, `gh repo view` and `gh api` GET, each naming the
//! watched repository exactly (`-R/--repo`, the repository positional of
//! `gh repo view`, or the `repos/<owner>/<name>/` path of `gh api`). One
//! command only: no chaining, pipes, substitution, redirects or env prefix.
//! Options are matched against a closed list; `--web`, request bodies,
//! headers, hosts and any method other than GET keep asking.
use crate::outside::shell;

/// Options of the listed `gh` view/list commands that take a value / none.
const VALUED: [&str; 18] = [
    "-R", "--repo", "-L", "--limit", "-s", "--state", "-l", "--label", "-A", "--author", "-a", "--assignee",
    "-S", "--search", "--json", "-q", "--jq", "--template",
];
const FLAGS: [&str; 5] = ["-c", "--comments", "--name-only", "--patch", "--required"];
/// `gh api` options allowed besides `-X/--method GET`.
const API_VALUED: [&str; 4] = ["-q", "--jq", "-t", "--template"];
const API_FLAGS: [&str; 4] = ["--paginate", "--slurp", "-i", "--include"];

/// Whether `line` is one read-only GitHub query on `repo`.
pub(crate) fn allowed(line: &str, repo: &str) -> bool {
    let Some(parsed) = shell::parse(line) else { return false };
    let [words] = parsed.segments.as_slice() else { return false };
    if parsed.heredoc_at.is_some() || line.contains(['>', '<', '\n']) {
        return false;
    }
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["gh", "issue", "list" | "view", rest @ ..] | ["gh", "pr", "list" | "view" | "diff" | "checks", rest @ ..] => {
            query(rest, repo, false)
        }
        ["gh", "repo", "view", rest @ ..] => query(rest, repo, true),
        ["gh", "api", rest @ ..] => api(rest, repo),
        _ => false,
    }
}

fn same(a: &str, b: &str) -> bool {
    crate::merge_policy::gate::repo_name(a).is_some_and(|a| a.eq_ignore_ascii_case(b))
}

/// `--name value` or `--name=value` split; `None` value for a bare word.
fn split(word: &str) -> (&str, Option<&str>) {
    match word.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value)),
        _ => (word, None),
    }
}

/// A view/list command: every `-R` names `repo`, at least one names it (or,
/// for `gh repo view`, the one positional does), positionals are numbers.
fn query(rest: &[&str], repo: &str, repo_positional: bool) -> bool {
    let (mut named, mut words) = (false, rest.iter());
    let mut positionals = Vec::new();
    while let Some(word) = words.next() {
        if !word.starts_with('-') {
            positionals.push(*word);
            continue;
        }
        let (name, inline) = split(word);
        if FLAGS.contains(&name) && inline.is_none() {
            continue;
        }
        if !VALUED.contains(&name) {
            return false;
        }
        let Some(value) = inline.or_else(|| words.next().copied()) else { return false };
        if matches!(name, "-R" | "--repo") {
            if !same(value, repo) {
                return false;
            }
            named = true;
        }
    }
    if repo_positional {
        return matches!(positionals.as_slice(), [only] if same(only, repo)) && !named;
    }
    named && positionals.len() <= 1 && positionals.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `gh api repos/<repo>/...` read with GET only.
fn api(rest: &[&str], repo: &str) -> bool {
    let (mut path, mut words) = (None, rest.iter());
    while let Some(word) = words.next() {
        if !word.starts_with('-') {
            if path.replace(*word).is_some() {
                return false;
            }
            continue;
        }
        let (name, inline) = split(word);
        if API_FLAGS.contains(&name) && inline.is_none() {
            continue;
        }
        let Some(value) = inline.or_else(|| words.next().copied()) else { return false };
        let ok = match name {
            "-X" | "--method" => value == "GET",
            _ => API_VALUED.contains(&name),
        };
        if !ok {
            return false;
        }
    }
    let Some(path) = path else { return false };
    let path = path.strip_prefix('/').unwrap_or(path);
    let Some(tail) = path.get(..6).filter(|p| *p == "repos/").map(|_| &path[6..]) else { return false };
    let mut parts = tail.splitn(3, '/');
    let (Some(owner), Some(name)) = (parts.next(), parts.next()) else { return false };
    let name = name.split('?').next().unwrap_or_default();
    same(&format!("{owner}/{name}"), repo) && !path.contains("..") && !path.contains(['{', ':', '#', '\\'])
}

#[cfg(test)]
mod tests {
    use super::allowed;
    const REPO: &str = "csheargm/hagency-demo-sandbox";

    #[test]
    fn read_only_queries_on_the_watched_repo_pass() {
        for line in [
            "gh issue list -R csheargm/hagency-demo-sandbox",
            "gh issue list --repo=csheargm/hagency-demo-sandbox --state open --json number,title",
            "gh issue view 12 -R CSheargm/Hagency-Demo-Sandbox --comments",
            "gh pr list -R csheargm/hagency-demo-sandbox -L 20",
            "gh pr diff 3 -R csheargm/hagency-demo-sandbox --name-only",
            "gh pr checks 3 --repo csheargm/hagency-demo-sandbox",
            "gh repo view csheargm/hagency-demo-sandbox --json name",
            "gh api 'repos/csheargm/hagency-demo-sandbox/issues?state=open' --paginate",
            "gh api -X GET /repos/csheargm/hagency-demo-sandbox/pulls/3/files --jq '.[].filename'",
        ] {
            assert!(allowed(line, REPO), "{line}");
        }
    }

    #[test]
    fn other_repos_writes_and_chains_ask() {
        for line in [
            "gh issue list -R other/repo",
            "gh issue list",
            "gh issue list -R csheargm/hagency-demo-sandbox -R other/repo",
            "gh issue list -R csheargm/hagency-demo-sandbox; rm -rf x",
            "gh issue list -R csheargm/hagency-demo-sandbox && gh issue close 1",
            "gh issue list -R csheargm/hagency-demo-sandbox | sh",
            "gh issue list -R csheargm/hagency-demo-sandbox > out.txt",
            "gh issue list -R $(echo csheargm/hagency-demo-sandbox)",
            "GH_HOST=evil gh issue list -R csheargm/hagency-demo-sandbox",
            "gh issue view 1 -R csheargm/hagency-demo-sandbox --web",
            "gh issue close 1 -R csheargm/hagency-demo-sandbox",
            "gh issue comment 1 -R csheargm/hagency-demo-sandbox -b hi",
            "gh pr merge 3 -R csheargm/hagency-demo-sandbox",
            "gh pr view https://github.com/other/repo/pull/1 -R csheargm/hagency-demo-sandbox",
            "gh repo view other/repo",
            "gh repo view",
            "gh api -X POST repos/csheargm/hagency-demo-sandbox/issues",
            "gh api --method=DELETE repos/csheargm/hagency-demo-sandbox/issues/1",
            "gh api repos/csheargm/hagency-demo-sandbox/issues -f title=x",
            "gh api repos/csheargm/hagency-demo-sandbox/issues --input body.json",
            "gh api repos/csheargm/hagency-demo-sandbox/issues -H X-HTTP-Method-Override:POST",
            "gh api repos/other/repo/issues",
            "gh api repos/csheargm/hagency-demo-sandbox/../../other/repo",
            "gh api graphql -f query=x",
            "gh api --hostname evil.example repos/csheargm/hagency-demo-sandbox",
        ] {
            assert!(!allowed(line, REPO), "{line}");
        }
    }
}
