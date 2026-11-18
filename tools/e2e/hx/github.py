"""GitHub reads and writes through the logged-in `gh` CLI (csheargm).

Only the sandbox repo is ever written, and only items carrying TEST_MARK."""
import base64
import functools
import json
import os
import re
import subprocess
import time

from . import config

GH_TIMEOUT_S = 60
READ_RETRIES = 3
RETRY_PAUSE_S = 5
TRANSIENT = ("TLS handshake timeout", "connection reset", "i/o timeout", "502 Bad Gateway", "EOF")
GH_ACCOUNT = "csheargm"  # gh's *active* account can change under us (seen 2026-11-17)


class GitHubError(RuntimeError):
    pass


def gh(*args: str, parse=True):
    cmd = ["gh", *args]
    if any("hagency-org" in a for a in args):
        raise GitHubError("hagency-org is off limits")
    tries = READ_RETRIES if is_read(args) else 1
    for attempt in range(tries):
        done = subprocess.run(cmd, capture_output=True, text=True, timeout=GH_TIMEOUT_S, env=_pinned_env())
        if done.returncode == 0:
            return json.loads(done.stdout) if parse and done.stdout.strip() else done.stdout
        if attempt + 1 < tries and any(t in done.stderr for t in TRANSIENT):
            time.sleep(RETRY_PAUSE_S)  # a read is safe to repeat; a write never is
            continue
        raise GitHubError(f"{' '.join(cmd[:4])}…: {done.stderr.strip()[:300]}")
    raise GitHubError("unreachable")


def is_read(args: tuple) -> bool:
    """gh calls that change nothing on GitHub."""
    if args[:1] == ("api",):
        return not any(a in ("-X", "--method", "-f", "-F", "--field", "--raw-field") for a in args)
    return len(args) > 1 and args[0] in ("pr", "issue") and args[1] in ("view", "list")


@functools.lru_cache(maxsize=1)
def _pinned_env() -> dict:
    """Environment that makes every gh call act as GH_ACCOUNT, whichever
    account is active. The token stays in this process's memory and env."""
    got = subprocess.run(["gh", "auth", "token", "--user", GH_ACCOUNT], capture_output=True,
                         text=True, timeout=GH_TIMEOUT_S)
    if got.returncode != 0 or not got.stdout.strip():
        raise GitHubError(f"gh has no login for {GH_ACCOUNT}")
    return {**os.environ, "GH_TOKEN": got.stdout.strip()}


def whoami() -> str:
    return gh("api", "user")["login"]


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
        gh("issue", "close", str(number), "-R", config.SANDBOX_REPO, "--comment", comment,
           "--reason", "not planned", parse=False)


def close_pr_if_unmerged(number: int, comment: str) -> str:
    """Close an unmerged PR for a runner-made issue and delete its branch."""
    data = pr(number)
    if data["state"] != "OPEN":
        return data["state"].lower()
    gh("pr", "close", str(number), "-R", config.SANDBOX_REPO, "--comment", comment,
       "--delete-branch", parse=False)
    return "closed"


def branches_naming_issue(number: int) -> list:
    """Non-default branches whose name carries "issue-N" / "issueN"."""
    names = gh("api", f"repos/{config.SANDBOX_REPO}/branches?per_page=100", "--jq", "[.[].name]")
    pattern = re.compile(rf"issue-?{number}(\b|[^0-9])")
    return [n for n in names if n not in ("main", "master") and pattern.search(n)]


def create_test_pr(branch: str, path: str, title: str) -> int:
    """A one-file `[e2e]` PR on a new branch from main (for review scenarios)."""
    _require_test_title(title)
    if not branch.startswith("e2e-"):
        raise GitHubError(f"test branches start with e2e-: {branch!r}")
    main = gh("api", f"repos/{config.SANDBOX_REPO}/git/ref/heads/main")["object"]["sha"]
    gh("api", "-X", "POST", f"repos/{config.SANDBOX_REPO}/git/refs", "-f", f"ref=refs/heads/{branch}", "-f", f"sha={main}")
    content = base64.b64encode(f"e2e review target {branch}\n".encode()).decode()
    gh("api", "-X", "PUT", f"repos/{config.SANDBOX_REPO}/contents/{path}", "-f", f"message={title}",
       "-f", f"content={content}", "-f", f"branch={branch}")
    url = gh("pr", "create", "-R", config.SANDBOX_REPO, "--head", branch, "--base", "main", "--title", title,
             "--body", "Automated end-to-end test PR (review target). Closed by the runner.", parse=False)
    return int(url.strip().rsplit("/", 1)[1])


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
