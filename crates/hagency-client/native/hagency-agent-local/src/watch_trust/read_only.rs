//! Read-only GitHub queries on a repository the Agent WATCHES from this Room
//! run without a card (ADR 0019): `gh issue list|view`, `gh pr
//! list|view|diff|checks`, and `gh api` GET, each naming the watched
//! repository exactly (`-R/--repo`, or the `repos/<owner>/<name>/` path of
//! `gh api`). They run with `GH_HOST=github.com` (`command`). One
//! command only: no chaining, pipes, substitution, redirects or env prefix.
//! Options are matched against a closed list; `--web`, request bodies,
//! headers, hosts, any method other than GET, free-form search and jq or
//! templates (gh's jq can read the token from its environment, GPT review)
//! keep asking. `gh api` reaches only the issue and pull request routes.
use crate::outside::shell;

/// Options of the listed `gh` view/list commands that take a value / none.
const VALUED: [&str; 13] = [
    "-R", "--repo", "-L", "--limit", "-s", "--state", "-l", "--label", "-A", "--author", "-a", "--assignee", "--json",
];
const FLAGS: [&str; 5] = ["-c", "--comments", "--name-only", "--patch", "--required"];
/// `gh api` options allowed besides `-X/--method GET`.
const API_FLAGS: [&str; 2] = ["--paginate", "-i"];

/// The command that runs: pinned to github.com whatever gh's config says
/// (GPT review), with git's helpers off.
pub(crate) fn command(line: &str) -> String {
    super::pin_env(&format!("GH_HOST='github.com' {line}"))
}

/// Whether `line` is one read-only GitHub query on `repo`.
pub(crate) fn allowed(line: &str, repo: &str) -> bool {
    let Some(parsed) = shell::parse(line) else { return false };
    let [words] = parsed.segments.as_slice() else { return false };
    // `gh` as written, never quoted or escaped: the token check before a
    // host run looks for that word, and must see it (GPT review P1).
    if parsed.heredoc_at.is_some() || line.contains(['>', '<', '\n']) || !line.starts_with("gh ") {
        return false;
    }
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["gh", "issue", "list" | "view", rest @ ..] | ["gh", "pr", "list" | "view" | "diff" | "checks", rest @ ..] => {
            query(rest, repo)
        }
        ["gh", "api", rest @ ..] => api(rest, repo),
        _ => false,
    }
}

/// The repository `line` reads, when it is one read-only query ([`allowed`]
/// for that repository): the `-R/--repo` value, or the `repos/<owner>/<name>`
/// of a `gh api` path. A read outside the room's set asks the owner on a card
/// naming exactly this (`codex::repo_cards`).
pub(crate) fn named_repo(line: &str) -> Option<String> {
    let parsed = shell::parse(line)?;
    let [words] = parsed.segments.as_slice() else { return None };
    let named = words.iter().enumerate().find_map(|(at, word)| match split(word) {
        ("-R" | "--repo", inline) => inline.or_else(|| words.get(at + 1).map(String::as_str)).map(str::to_owned),
        _ => {
            let mut parts = word.strip_prefix('/').unwrap_or(word).strip_prefix("repos/")?.split(['/', '?']);
            Some(format!("{}/{}", parts.next()?, parts.next()?))
        }
    })?;
    allowed(line, &named).then_some(named)
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

/// A view/list command: every `-R` names `repo`, at least one does, and the
/// one positional (if any) is a number.
fn query(rest: &[&str], repo: &str) -> bool {
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
        if !matches!(name, "-X" | "--method") || value != "GET" {
            return false;
        }
    }
    path.is_some_and(|path| api_route(path, repo))
}

/// `repos/<repo>/(issues|pulls)[/<n>[/comments|/files|/reviews]]`, with an
/// optional query of plain `key=value` pairs.
fn api_route(path: &str, repo: &str) -> bool {
    let path = path.strip_prefix('/').unwrap_or(path);
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let query_ok = query.split('&').filter(|q| !q.is_empty()).all(|pair| {
        pair.split_once('=').is_some_and(|(k, v)| {
            ["state", "per_page", "page", "labels", "sort", "direction"].contains(&k) && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b',' || b == b'-' || b == b'_')
        })
    });
    let parts: Vec<&str> = route.split('/').collect();
    let number = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    let shape = match parts.as_slice() {
        ["repos", _, _, "issues" | "pulls"] => true,
        ["repos", _, _, "issues" | "pulls", n] => number(n),
        ["repos", _, _, "issues", n, "comments"] => number(n),
        ["repos", _, _, "pulls", n, "comments" | "files" | "reviews"] => number(n),
        _ => false,
    };
    shape && query_ok && same(&format!("{}/{}", parts[1], parts[2]), repo)
}

#[cfg(test)]
mod tests {
    use super::allowed;
    const REPO: &str = "csheargm/hagency-demo-sandbox";

    /// Live PR #135, Nov 19: a review request's linked-issue read must run
    /// card-free like its pull request reads, for Claude and Codex reviewers.
    #[test]
    fn a_review_requests_reads_and_issue_read_pass() {
        let fill = |text: &str| text.replace("<N>", "135").replace("<I>", "134").replace("<owner/repo>", REPO);
        let reads = fill(hagency_runtime::REVIEW_READS);
        let issue = fill(hagency_runtime::ISSUE_READ);
        let lines: Vec<&str> = reads.split(": ").nth(1).unwrap().split(" and ").chain(issue.split(" and ").skip(1)).collect();
        assert_eq!(lines.len(), 3, "{lines:?}");
        for line in lines {
            assert!(allowed(line, REPO), "{line}");
        }
    }

    #[test]
    fn read_only_queries_on_the_watched_repo_pass() {
        for line in [
            "gh issue list -R csheargm/hagency-demo-sandbox",
            "gh issue list --repo=csheargm/hagency-demo-sandbox --state open --json number,title",
            "gh issue view 12 -R CSheargm/Hagency-Demo-Sandbox --comments",
            "gh pr list -R csheargm/hagency-demo-sandbox -L 20",
            "gh pr diff 3 -R csheargm/hagency-demo-sandbox --name-only",
            "gh pr view 3 -R csheargm/hagency-demo-sandbox --json title,state",
            "gh pr checks 3 --repo csheargm/hagency-demo-sandbox",
            "gh api 'repos/csheargm/hagency-demo-sandbox/issues?state=open' --paginate",
            "gh api -X GET /repos/csheargm/hagency-demo-sandbox/pulls/3/files",
            "gh api repos/csheargm/hagency-demo-sandbox/issues/4/comments",
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
            "'gh' pr view 3 -R csheargm/hagency-demo-sandbox",
            "\\gh pr view 3 -R csheargm/hagency-demo-sandbox",
            " gh pr view 3 -R csheargm/hagency-demo-sandbox",
            "GH_HOST=evil gh issue list -R csheargm/hagency-demo-sandbox",
            "gh issue view 1 -R csheargm/hagency-demo-sandbox --web",
            "gh issue close 1 -R csheargm/hagency-demo-sandbox",
            "gh issue comment 1 -R csheargm/hagency-demo-sandbox -b hi",
            "gh pr merge 3 -R csheargm/hagency-demo-sandbox",
            "gh pr view https://github.com/other/repo/pull/1 -R csheargm/hagency-demo-sandbox",
            "gh repo view csheargm/hagency-demo-sandbox --json parent",
            "gh issue list -R github.example.com/csheargm/hagency-demo-sandbox",
            "gh api -X POST repos/csheargm/hagency-demo-sandbox/issues",
            "gh api --method=DELETE repos/csheargm/hagency-demo-sandbox/issues/1",
            "gh api repos/csheargm/hagency-demo-sandbox/issues -f title=x",
            "gh api repos/csheargm/hagency-demo-sandbox/issues --input body.json",
            "gh api repos/csheargm/hagency-demo-sandbox/issues -H X-HTTP-Method-Override:POST",
            "gh api repos/other/repo/issues",
            "gh api repos/csheargm/hagency-demo-sandbox/../../other/repo",
            "gh api graphql -f query=x",
            "gh api --hostname evil.example repos/csheargm/hagency-demo-sandbox",
            // GPT review: gh's jq reads the environment (the token).
            "gh issue list -R csheargm/hagency-demo-sandbox --jq env",
            "gh api repos/csheargm/hagency-demo-sandbox/issues -q env.GH_TOKEN",
            "gh issue list -R csheargm/hagency-demo-sandbox --template x",
            // GPT review: free-form search can reach other repositories.
            "gh issue list -R csheargm/hagency-demo-sandbox --search 'repo:other/repo'",
            // GPT review: only issue and pull request routes.
            "gh api repos/csheargm/hagency-demo-sandbox/contents/secrets.txt",
            "gh api repos/csheargm/hagency-demo-sandbox/actions/runs",
            "gh api repos/csheargm/hagency-demo-sandbox/issues/%2e%2e",
            "gh api 'repos/csheargm/hagency-demo-sandbox/issues?q=repo:other/repo'",
        ] {
            assert!(!allowed(line, REPO), "{line}");
        }
    }

    /// A card for a read outside the room's set names the repository the
    /// query reads; anything that is not one read-only query names none.
    #[test]
    fn a_read_names_its_repository_and_nothing_else_does() {
        use super::named_repo;
        assert_eq!(named_repo("gh pr view 111 --repo other/repo --json title,files").as_deref(), Some("other/repo"));
        assert_eq!(named_repo("gh issue list -R=other/repo").as_deref(), None, "-R takes no =");
        assert_eq!(named_repo("gh pr diff 3 --repo=other/repo").as_deref(), Some("other/repo"));
        assert_eq!(named_repo("gh api /repos/other/repo/pulls/3/files").as_deref(), Some("other/repo"));
        for line in ["gh pr comment 3 --repo other/repo -b hi", "gh pr view 3 --repo a/b --repo c/d", "gh pr view 3", "gh api repos/a/b/contents/x", "gh pr view 3 --repo a/b; rm -rf x"] {
            assert_eq!(named_repo(line), None, "{line}");
        }
    }
}
