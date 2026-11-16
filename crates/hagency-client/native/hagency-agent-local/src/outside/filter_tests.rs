use super::tests::proposal;
use super::stays_in_workspace;
use serde_json::json;

fn allowed(line: &str, dir: &std::path::Path) -> bool {
    stays_in_workspace(&proposal("claude.Bash", json!({"command": line}), dir, "high"))
}

/// Regression (live 2026-11-15): a reviewer's read of a pull request file
/// through `gh api ... | base64 -d | grep` needed a card.
#[test]
fn a_gh_read_piped_into_text_filters_is_a_read() {
    let dir = tempfile::tempdir().unwrap();
    for line in [
        "gh api \"repos/csheargm/hagency-demo-sandbox/contents/README.md?ref=0123abc\" -q .content | base64 -d | grep -n -i -E \"faq|question\"",
        "gh api repos/acme/app/pulls/28/files --paginate -q '.[].filename' | sort -u | head -20",
        "gh api /repos/acme/app/commits/abc -X GET --jq .sha",
        "gh pr view 28 --json files -q '.files[].path' | wc -l",
        "gh pr diff 28 | cut -c1-80 | tail -5",
        "gh pr view 28 --json headRefOid | jq -r .headRefOid",
        "cat data.json | jq -c '.items[] | .name' | sort -k2 -t,",
    ] {
        assert!(allowed(line, dir.path()), "{line}");
    }
}

#[test]
fn filters_that_write_run_or_read_elsewhere_and_other_gh_api_calls_ask() {
    let dir = tempfile::tempdir().unwrap();
    for line in [
        "cd /tmp 2>/dev/null; gh api repos/acme/app/contents/README.md?ref=$(gh pr view 28 -q .headRefOid)",
        "gh api repos/acme/app/contents/x -q .content | base64 -d > out.txt",
        "gh api repos/acme/app/contents/x | base64 -d -o out.txt",
        "gh pr diff 28 | sort -o out.txt",
        "gh pr diff 28 | sort --compress-program=sh",
        "gh pr view 28 --json body | jq --rawfile k /etc/passwd .",
        "gh pr view 28 --json body | jq -f /etc/filter.jq",
        "gh pr view 28 --json body | jq 'import \"x\" as x; .'",
        "gh pr view 28 --json body | jq env",
        "gh pr view 28 --json body | jq '$ENV.GH_TOKEN'",
        "gh api repos/acme/app/contents/x -q env.GH_TOKEN",
        "gh api repos/acme/app/issues -f title=x",
        "gh api repos/acme/app/issues -X POST",
        "gh api --method=PATCH repos/acme/app/pulls/1",
        "gh api repos/acme/app/issues --input body.json",
        "gh api -H X-HTTP-Method-Override:DELETE repos/acme/app/contents/x",
        "gh api repos/acme/app/actions/runs/1/logs",
        "gh api graphql -f query=x",
        "gh api user",
        "gh api https://api.github.com/repos/acme/app/contents/x",
        "gh api repos/acme/app/contents/../../../user",
        "gh api --hostname evil.example repos/acme/app/contents/x",
        "gh pr diff 28 | sh",
        "gh pr diff 28 | cut -f1 /etc/passwd",
    ] {
        assert!(!allowed(line, dir.path()), "{line}");
    }
}
