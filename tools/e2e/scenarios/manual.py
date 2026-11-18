"""Scenarios 9–10: opt-in failure injection and owner-only Agent lifecycle.

Injection never changes the system (no network, DNS or firewall edits). The one
automated injection pauses Betty's demo window with SIGSTOP and always resumes
it with SIGCONT in a `finally`, so it is reversible even when the check fails."""
import os
import signal
import subprocess
import time

from hx import config, personas
from hx.scenario import FAIL, OPT_IN, PASS, Result, blocked, mention_content, now_ms, reply_with, server_ts

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
    pid = _betty_pid()
    r, t0 = Result(name), time.time()
    token = f"E2E-{ctx.run_id}-s9"
    os.kill(pid, signal.SIGSTOP)
    try:
        ev = personas.persona("zz-betty").send(ctx.room, mention_content(
            f"{config.TEST_MARK} reply with the word {token}", ["bird-claude"]))
        r.evidence.append(f"asked {ev} while pid {pid} stopped for {PAUSE_S}s")
        time.sleep(PAUSE_S)
    finally:
        os.kill(pid, signal.SIGCONT)
    resumed_ms = now_ms()
    reply = reply_with("zz-betty", ctx.room, "bird-claude", server_ts("zz-betty", ctx.room, ev),
                       token, RESUME_ANSWER_TIMEOUT_S)
    r.evidence.append(f"reply after resume: {reply['event_id'] if reply else 'none'}")
    ok = bool(reply) and reply["origin_server_ts"] >= resumed_ms - 5000
    if reply and not ok:
        r.note = "answered while the owner's window was paused: the answer did not come from that window"
    r.status, r.seconds, r.kind = (PASS if ok else FAIL), time.time() - t0, "" if ok else "finding"
    return r


def _betty_pid() -> int:
    """Betty's window pid, only if that pid really is her demo window."""
    pid = int(BETTY_PID_FILE.read_text().strip())
    cmd = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True).stdout
    if pid <= 1 or "fresh-betty" not in cmd:
        raise RuntimeError(f"{BETTY_PID_FILE} pid {pid} is not Betty's window; not signalling it")
    return pid


def s10_agent_lifecycle(ctx) -> Result:
    name = "10 agent add / remove / Leave→Message"
    return Result(name, BLOCKED_MANUAL, note=(
        "add is exercised by the owner UI setup (bridge: + → Add agent to this room → pick → Invite → "
        "Allow in group rooms); remove and Leave→Message are owner-UI only and not driven yet (their "
        "widget ids are not mapped). There is no external owner endpoint for them."))

