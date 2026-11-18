"""GitHub reads and writes through the logged-in `gh` CLI (csheargm).

Only the sandbox repo is ever written, and only items carrying TEST_MARK."""
import json
import re
import subprocess

from . import config

GH_TIMEOUT_S = 60


class GitHubError(RuntimeError):
    pass


def gh(*args: str, parse=True):
    cmd = ["gh", *args]
    if any("hagency-org" in a for a in args):
        raise GitHubError("hagency-org is off limits")
    done = subprocess.run(cmd, capture_output=True, text=True, timeout=GH_TIMEOUT_S)
    if done.returncode != 0:
        raise GitHubError(f"{' '.join(cmd[:4])}…: {done.stderr.strip()[:300]}")
    return json.loads(done.stdout) if parse and done.stdout.strip() else done.stdout


def _require_test_title(title: str):
    if not title.startswith(config.TEST_MARK):
        raise GitHubError(f"refusing to write an item without {config.TEST_MARK}: {title!r}")


def create_issue(title: str, body: str) -> int:
    _require_test_title(title)
    url = gh("issue", "create", "-R", config.SANDBOX_REPO, "--title", title, "--body", body, parse=False)
    return int(url.strip().rsplit("/", 1)[1])


def issue(number: int) -> dict:
    return gh("issue", "view", str(number), "-R", config.SANDBOX_REPO,
              "--json", "number,title,state,closedAt,createdAt,comments,url")


def closes(body: str, number: int) -> bool:
    """Whether a PR body links issue `number` with a closing keyword."""
    return bool(re.search(rf"\b(close[sd]?|fix(e[sd])?|resolve[sd]?) #{number}\b", body or "", re.IGNORECASE))


def prs_closing(number: int) -> list:
    """PRs whose body says "Closes #N" (any state), newest 100 scanned."""
    rows = gh("pr", "list", "-R", config.SANDBOX_REPO, "--state", "all", "-L", "100",
              "--json", "number,title,body,state,headRefName,createdAt,mergedAt,url,reviews,comments")
    return [p for p in rows if closes(p.get("body"), number)]


def pr(number: int) -> dict:
    return gh("pr", "view", str(number), "-R", config.SANDBOX_REPO, "--json",
              "number,title,state,headRefName,createdAt,mergedAt,url,reviews,comments,body")


def close_issue(number: int, comment: str):
    data = issue(number)
    _require_test_title(data["title"])
    if data["state"] == "OPEN":
        gh("issue", "close", str(number), "-R", config.SANDBOX_REPO, "--comment", comment, parse=False)


def close_pr_if_unmerged(number: int, comment: str) -> str:
    """Close an unmerged PR for a runner-made issue and delete its branch."""
    data = pr(number)
    if data["state"] != "OPEN":
        return data["state"].lower()
    gh("pr", "close", str(number), "-R", config.SANDBOX_REPO, "--comment", comment,
       "--delete-branch", parse=False)
    return "closed"


def branch_exists(name: str) -> bool:
    try:
        gh("api", f"repos/{config.SANDBOX_REPO}/branches/{name}")
        return True
    except GitHubError as e:
        if "404" in str(e) or "Not Found" in str(e):
            return False
        raise


def delete_branch(name: str):
    if name in ("main", "master") or not name:
        raise GitHubError(f"refusing to delete branch {name!r}")
    gh("api", "-X", "DELETE", f"repos/{config.SANDBOX_REPO}/git/refs/heads/{name}", parse=False)
