"""Scenarios 7–8: the autonomous GitHub issue loop and a cross-model review."""
import time
from datetime import datetime

from hx import config, evidence, github, personas
from hx.scenario import FAIL, PASS, Result, blocked, first_reply, mention_content, now_ms, wait_for

LOOP_TIMEOUT_S = 25 * 60
LOOP_POLL_S = 20
REVIEW_TIMEOUT_S = 600
LOOP_AGENT = "bird-claude"  # its issue watch covers SANDBOX_REPO (mode "all")
STAGES = ("created", "handed", "pr_opened", "reviewed", "merged", "closed")


def _ts(iso: str | None) -> float | None:
    return datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp() if iso else None


def _stages(number: int) -> dict:
    """Wall-clock time of each loop stage reached so far for one issue."""
    issue = github.issue(number)
    st = {"created": _ts(issue["createdAt"]), "closed": _ts(issue.get("closedAt"))}
    rows = evidence.ledger("betty", "select state, handed_at, pr from issue_track where agent=? and repo=? "
                           "and number=?", (evidence.agent_id(LOOP_AGENT), config.SANDBOX_REPO, number))
    st["ledger_state"] = rows[0][0] if rows else None
    st["handed"] = float(rows[0][1]) if rows else None
    prs = github.prs_closing(number)
    if prs:
        p = sorted(prs, key=lambda x: x["createdAt"])[0]
        st.update(pr=p["number"], pr_url=p["url"], pr_opened=_ts(p["createdAt"]),
                  merged=_ts(p.get("mergedAt")), branch=p["headRefName"])
        reviews = [x for x in p.get("reviews", []) if x.get("submittedAt")]
        st["reviewed"] = min((_ts(x["submittedAt"]) for x in reviews), default=None)
    return st


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
    return " ".join(parts)


def s7_issue_loop(ctx) -> Result:
    name = "7 issue loop ×2 in parallel: issue→hand-off→PR→review→merge→closed"
    r, t0 = Result(name), time.time()
    numbers = [_open_issue(ctx, "a"), _open_issue(ctx, "b")]
    done = lambda st: all(st.get(s) for s in STAGES)  # noqa: E731
    states = {}

    def probe():
        for n in numbers:
            states[n] = _stages(n)
        return all(done(states[n]) for n in numbers)

    wait_for(probe, LOOP_TIMEOUT_S, LOOP_POLL_S)
    r.evidence = [_describe(n, states[n]) for n in numbers]
    ok = all(done(states[n]) for n in numbers)
    if not ok:
        r.note = f"not every stage reached within {LOOP_TIMEOUT_S // 60} min"
    ctx.loop_states = states
    r.status, r.seconds, r.kind = (PASS if ok else FAIL), time.time() - t0, "" if ok else "finding"
    return r


def s8_cross_model_review(ctx) -> Result:
    name = "8 cross-model review (Anna-side asks bird-codex to review a PR)"
    if (b := blocked(name, ctx, ["bird-codex"])):
        return b
    prs = [st["pr"] for st in ctx.loop_states.values() if st.get("pr")]
    if not prs:
        return Result(name, FAIL, note="no PR from scenario 7 to review", kind="dependency")
    r, t0 = Result(name), time.time()
    number = prs[0]
    before = len(github.pr(number)["reviews"]) + len(github.pr(number)["comments"])
    since = now_ms()
    ev = personas.persona("zz-anna").send(ctx.room, mention_content(
        f"{config.TEST_MARK} please review PR #{number} in {config.SANDBOX_REPO} and post your review "
        "on GitHub.", ["bird-codex"]))
    r.evidence.append(f"ask {ev} for PR #{number}")
    reply = first_reply("zz-anna", ctx.room, "bird-codex", since, REVIEW_TIMEOUT_S)
    posted = wait_for(lambda: len(github.pr(number)["reviews"]) + len(github.pr(number)["comments"]) > before, 120)
    r.evidence.append(f"room reply {reply['event_id'] if reply else 'none'}; GitHub review posted: {bool(posted)}")
    ok = bool(reply) and bool(posted)
    if not ok:
        r.note = "no room answer" if not reply else "answered in room but nothing posted on the PR"
    r.status, r.seconds, r.kind = (PASS if ok else FAIL), time.time() - t0, "" if ok else "finding"
    return r


def cleanup(ctx) -> list:
    """Close runner-made issues/PRs that did not merge; delete runner-made branches."""
    done = []
    for st in ctx.loop_states.values():
        if st.get("pr") and not st.get("merged"):
            done.append(f"PR #{st['pr']}: {github.close_pr_if_unmerged(st['pr'], 'e2e cleanup: not merged')}")
        if st.get("merged") and st.get("branch") and github.branch_exists(st["branch"]):
            github.delete_branch(st["branch"])
            done.append(f"deleted merged branch {st['branch']}")
    for n in ctx.created_issues:
        github.close_issue(n, "e2e cleanup: closed by the test runner")
        done.append(f"issue #{n} closed/kept closed")
    for b in ctx.created_branches:
        if github.branch_exists(b):
            github.delete_branch(b)
            done.append(f"deleted branch {b}")
    return done
