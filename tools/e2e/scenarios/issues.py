"""Scenarios 7–8: the autonomous GitHub issue loop and a cross-model review."""
import time
from datetime import datetime

from hx import config, evidence, github, owner_ui, personas
from hx.bridge import BridgeDown, StepNotFound
from hx.scenario import FAIL, PASS, Result, blocked, first_reply, mention_content, now_ms, wait_for

LOOP_TIMEOUT_S = 25 * 60
LOOP_POLL_S = 20
REVIEW_TIMEOUT_S = 600
LOOP_AGENT = "bird-claude"  # its issue watch covers SANDBOX_REPO (mode "all")
STAGES = ("created", "handed", "pr_opened", "reviewed", "merged", "closed")
CLOCK_SLACK_S = 5  # GitHub and the ledger use different clocks


def _ts(iso: str | None) -> float | None:
    return datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp() if iso else None


def _ledger_row(number: int):
    rows = evidence.ledger("betty", "select state, handed_at, pr from issue_track where agent=? and repo=? "
                           "and number=?", (evidence.agent_id(LOOP_AGENT), config.SANDBOX_REPO, number))
    return rows[0] if rows else (None, None, None)


def _loop_pr(number: int, ledger_pr):
    """The PR the ledger recorded for this issue, else the only PR closing it."""
    if ledger_pr:
        return github.pr(int(ledger_pr))
    prs = github.prs_closing(number)
    return prs[0] if len(prs) == 1 else None


def _stages(number: int) -> dict:
    """Wall-clock time of each loop stage reached so far for one issue."""
    issue = github.issue(number)
    st = {"created": _ts(issue["createdAt"]), "issue_state": issue["state"],
          "closed": _ts(issue.get("closedAt")) if issue["state"] == "CLOSED" else None}
    state, handed, ledger_pr = _ledger_row(number)
    st["ledger_state"], st["handed"] = state, float(handed) if handed else None
    p = _loop_pr(number, ledger_pr)
    if p:
        st.update(pr=p["number"], pr_url=p["url"], pr_opened=_ts(p["createdAt"]),
                  merged=_ts(p.get("mergedAt")), branch=p["headRefName"], pr_body=p.get("body") or "")
        reviews = [x for x in p.get("reviews", []) if x.get("submittedAt")]
        st["reviewed"] = min((_ts(x["submittedAt"]) for x in reviews), default=None)
    else:
        st["pushed_no_pr"] = github.branches_naming_issue(number)
    return st


def _order_problems(st: dict) -> list:
    """Stages present but out of order (e.g. reviewed after merge)."""
    times = [(s, st.get(s)) for s in STAGES]
    return [f"{a} after {b}" for (b, tb), (a, ta) in zip(times, times[1:])
            if ta and tb and ta + CLOCK_SLACK_S < tb]


def _complete(st: dict) -> bool:
    return all(st.get(s) for s in STAGES) and not _order_problems(st)


def _open_issue(ctx, label: str) -> int:
    path = f"e2e/{ctx.run_id}-{label}.md"
    number = github.create_issue(
        f"{config.TEST_MARK} docs: add {path}",
        f"Create the file `{path}` containing the single line `e2e check {ctx.run_id} {label}`. "
        "Change nothing else. This is an automated end-to-end test issue.")
    ctx.created_issues.append(number)
    return number


def _describe(number: int, st: dict) -> str:
    t0 = st["created"]
    parts = [f"#{number}"] + [f"{s}+{st[s] - t0:.0f}s" for s in STAGES[1:] if st.get(s)]
    missing = [s for s in STAGES[1:] if not st.get(s)]
    if st.get("pr_url"):
        parts.append(st["pr_url"])
    if missing:
        parts.append(f"missing: {', '.join(missing)} (ledger state {st.get('ledger_state')})")
    parts += [f"order: {p}" for p in _order_problems(st)]
    if st.get("pushed_no_pr"):
        parts.append(f"branch pushed but no PR: {', '.join(st['pushed_no_pr'])}")
    return " ".join(parts)


def s7_issue_loop(ctx) -> Result:
    name = "7 issue loop ×2 in parallel: issue→hand-off→PR→review→merge→closed"
    r, t0 = Result(name), time.time()
    numbers = [_open_issue(ctx, "a"), _open_issue(ctx, "b")]
    states = ctx.loop_states  # shared at once so cleanup sees PRs even if this raises

    def probe():
        for n in numbers:
            states[n] = _stages(n)
        return all(_complete(states[n]) for n in numbers)

    wait_for(probe, LOOP_TIMEOUT_S, LOOP_POLL_S)
    r.evidence = [_describe(n, states[n]) for n in numbers]
    ok = all(_complete(states[n]) for n in numbers)
    if not ok and ctx.read_dev_room:
        r.evidence.append(_agent_said(numbers))
        r.note = f"not every stage reached in order within {LOOP_TIMEOUT_S // 60} min"
    r.status, r.seconds, r.kind = (PASS if ok else FAIL), time.time() - t0, "" if ok else "finding"
    return r


def _agent_said(numbers: list) -> str:
    """What the loop agent told the dev room about these issues, read from
    Betty's window (read only; zz users are not dev-room members)."""
    try:
        texts = owner_ui.visible_texts("betty", "dev room", tuple(f"#{n}" for n in numbers))
    except (BridgeDown, StepNotFound) as e:
        return f"dev room not read: {e}"
    return "dev room (Betty's window): " + " / ".join(t[:200] for t in texts[-6:]) if texts \
        else "dev room (Betty's window): nothing visible mentions these issues"


def _activity_since(number: int, since_s: float) -> list:
    """Reviews and comments on the PR created after `since_s` (one snapshot)."""
    p = github.pr(number)
    items = [("review", x.get("submittedAt"), x.get("body", "")) for x in p.get("reviews", [])]
    items += [("comment", x.get("createdAt"), x.get("body", "")) for x in p.get("comments", [])]
    return [(k, t, b) for k, t, b in items if t and _ts(t) >= since_s - CLOCK_SLACK_S and b.strip()]


def s8_cross_model_review(ctx) -> Result:
    name = "8 cross-model review (Anna-side asks bird-codex to review a PR)"
    if (b := blocked(name, ctx, ["bird-codex"])):
        return b
    r, t0 = Result(name), time.time()
    prs = [st["pr"] for st in ctx.loop_states.values() if st.get("pr")]
    if not prs:  # scenario 7 not run: review a PR of the runner's own
        branch = f"e2e-{ctx.run_id}-s8"
        ctx.created_branches.append(branch)
        prs = [github.create_test_pr(branch, f"e2e/{branch}.md", f"{config.TEST_MARK} docs: add e2e/{branch}.md")]
        ctx.created_prs.append(prs[0])
        r.evidence.append(f"review target: runner PR #{prs[0]}")
    number, since = prs[0], now_ms()
    ev = personas.persona("zz-anna").send(ctx.room, mention_content(
        f"{config.TEST_MARK} please review PR #{number} in {config.SANDBOX_REPO} and post your review "
        "on GitHub.", ["bird-codex"]))
    r.evidence.append(f"ask {ev} for PR #{number}")
    reply = first_reply("zz-anna", ctx.room, "bird-codex", since, REVIEW_TIMEOUT_S)
    posted = wait_for(lambda: _activity_since(number, since / 1000), 120)
    r.evidence.append(f"room reply {reply['event_id'] if reply else 'none'}; "
                      f"GitHub after ask: {[(k, t) for k, t, _ in posted or []]}")
    ok = bool(reply) and bool(posted)
    if not ok:
        r.note = "no room answer" if not reply else \
            f"answered in room but nothing new posted on the PR; bird-codex said: {reply['content'].get('body', '')[:240]!r}"
    r.status, r.seconds, r.kind = (PASS if ok else FAIL), time.time() - t0, "" if ok else "finding"
    return r


def _is_test_pr(st: dict, issue: int) -> bool:
    """Only a PR that names a runner-made issue, from a non-default branch."""
    return bool(st.get("branch")) and st["branch"] not in ("main", "master") and \
        github.closes(st.get("pr_body", ""), issue)


def cleanup(ctx) -> list:
    """Close runner-made issues/PRs that did not merge; delete runner-made branches.
    Every step is attempted; failures are reported, not swallowed."""
    done = []
    for issue, st in ctx.loop_states.items():
        if st.get("pr") and _is_test_pr(st, issue):
            done.append(_try(f"PR #{st['pr']}", lambda s=st: _settle_pr(s)))
    for issue, st in ctx.loop_states.items():
        if not st.get("pr"):
            for b in github.branches_naming_issue(issue):
                done.append(_try(f"branch {b} (no PR)", lambda b=b: github.delete_branch(b) or "deleted"))
    for pr in ctx.created_prs:
        done.append(_try(f"PR #{pr}", lambda pr=pr: github.close_pr_if_unmerged(pr, "e2e cleanup: review target")))
    for n in ctx.created_issues:
        done.append(_try(f"issue #{n}", lambda n=n: github.close_issue(n, "e2e cleanup: closed by the test runner")
                         or "closed (or already closed)"))
    for b in ctx.created_branches:
        done.append(_try(f"branch {b}", lambda b=b: github.delete_branch(b) or "deleted"
                         if github.branch_exists(b) else "absent"))
    return done


def _settle_pr(st: dict) -> str:
    if not st.get("merged"):
        return github.close_pr_if_unmerged(st["pr"], "e2e cleanup: not merged")
    if github.branch_exists(st["branch"]):
        github.delete_branch(st["branch"])
        return f"merged, kept; deleted head branch {st['branch']}"
    return "merged, kept"


def _try(label: str, step) -> str:
    try:
        return f"{label}: {step()}"
    except Exception as e:  # noqa: BLE001 — recorded in the report's Cleanup section as a failure
        return f"{label}: CLEANUP FAILED: {str(e)[:200]}"
