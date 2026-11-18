"""Scenarios 9–10: opt-in failure injection and owner-only Agent lifecycle.

Injection never changes the system (no network, DNS or firewall edits). The one
automated injection pauses Betty's demo window with SIGSTOP and always resumes
it with SIGCONT in a `finally`, so it is reversible even when the check fails."""
import os
import signal
import time

from hx import config, personas
from hx.scenario import FAIL, OPT_IN, PASS, Result, blocked, first_reply, mention_content, now_ms

BLOCKED_MANUAL = "MANUAL"
PAUSE_S = 45
RESUME_ANSWER_TIMEOUT_S = 300
BETTY_PID_FILE = config.RUN / "fresh-betty" / "desktop.pid"

MANUAL_INJECTIONS = [
    ("pause-betty (automated with --inject pause-betty)",
     f"kill -STOP $(cat {BETTY_PID_FILE}); sleep {PAUSE_S}; kill -CONT $(cat {BETTY_PID_FILE})",
     "a question to bird-claude asked while paused is answered after resume, not lost"),
    ("restart Betty's window mid-answer",
     f"{config.ROOT}/int-demo-up.sh --fresh betty beta --stop && {config.ROOT}/int-demo-up.sh --fresh betty beta",
     "the in-flight run ends with a visible failure or resumes; no silent drop"),
    ("Codex CLI missing for bird-codex",
     "start Betty's window with HAGENCY_CODEX_BINARY=/nonexistent (edit int-demo-up.sh locally, then revert)",
     "bird-codex answers with a clear setup error instead of silence"),
]


def s9_failure_injection(ctx, enabled: bool) -> Result:
    name = "9 failure injection: Betty's window paused during a question"
    listing = "; ".join(f"[{n}] `{cmd}` expect: {exp}" for n, cmd, exp in MANUAL_INJECTIONS)
    if not enabled:
        return Result(name, OPT_IN, note=f"default off. Run with --inject pause-betty. Manual list: {listing}")
    if (b := blocked(name, ctx, ["bird-claude"])):
        return b
    pid = int(BETTY_PID_FILE.read_text().strip())
    r, t0 = Result(name), time.time()
    since = now_ms()
    os.kill(pid, signal.SIGSTOP)
    try:
        ev = personas.persona("zz-betty").send(ctx.room, mention_content(
            f"{config.TEST_MARK} reply with the word E2E-{ctx.run_id}-s9", ["bird-claude"]))
        r.evidence.append(f"asked {ev} while pid {pid} stopped for {PAUSE_S}s")
        time.sleep(PAUSE_S)
    finally:
        os.kill(pid, signal.SIGCONT)
    reply = first_reply("zz-betty", ctx.room, "bird-claude", since, RESUME_ANSWER_TIMEOUT_S)
    r.evidence.append(f"reply after resume: {reply['event_id'] if reply else 'none'}")
    r.status, r.seconds = (PASS if reply else FAIL), time.time() - t0
    r.kind = "" if reply else "finding"
    return r


def s10_agent_lifecycle(ctx) -> Result:
    name = "10 agent add / remove / Leave→Message"
    return Result(name, BLOCKED_MANUAL, note=(
        "not API-drivable: binding, removing or pausing an Agent in a room is an owner command that the "
        "desktop executes in-process (console/api/owned-agents/{id}/bindings via the native owner engine); "
        "the server refuses a bare puppet invite, and there is no external owner endpoint. Needs a test hook "
        "(see README 'Gaps') or a UI driver."))

