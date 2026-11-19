"""Is each Agent's model provider usable? A signed-out Claude Code login makes
every Claude Agent silent; that is a missing precondition (BLOCKED), not a
product finding (FAIL), and never a pass (R14).

Two signals, either one blocks:
- `claude auth status` (JSON; only `loggedIn` is read, nothing is printed);
- the owner's desktop log: the agent runtime's CLAUDE_LOGIN_REQUIRED profile
  error (hagency-agent-local runtime.rs) in the last few minutes."""
import json
import re
import subprocess
from datetime import datetime, timedelta, timezone

from . import config, evidence
from .scenario import BLOCKED, FAIL

CLAUDE_LOGIN_REQUIRED = "dedicated owner Claude login required"  # mirrors runtime.rs CLAUDE_LOGIN_REQUIRED
STARTUP_CHECK_FAILED = "Local provider start-up check failed"
SIGNED_OUT_NOTE = "agent signed out (Claude login)"
RECENT_WINDOW = timedelta(minutes=5)
AUTH_STATUS_TIMEOUT_S = 20
NO_ANSWER_MARKS = ("(nothing)", "no answer")
_ANSI = re.compile(r"\x1b\[[0-9;]*m")
_TS = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.\d+)?Z")


def claude_agents() -> list:
    return [a for a in config.AGENTS if config.AGENT_PROVIDERS[a] == "claude"]


def is_login_error(line: str) -> bool:
    """A desktop-log line saying the owner's Claude login is missing."""
    text = _ANSI.sub("", line)
    return CLAUDE_LOGIN_REQUIRED in text or (STARTUP_CHECK_FAILED in text and "Claude" in text)


def line_time(line: str) -> datetime | None:
    """UTC timestamp a tracing line starts with (ANSI colour codes ignored)."""
    m = _TS.match(_ANSI.sub("", line).lstrip())
    return datetime.strptime(m.group(1), "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc) if m else None


def recent_login_errors(text: str, now: datetime, window: timedelta = RECENT_WINDOW) -> list:
    """Login-error lines stamped within `window` before `now` (undated lines skipped)."""
    hits = []
    for line in text.splitlines():
        ts = line_time(line)
        if ts is not None and now - window <= ts <= now + timedelta(minutes=1) and is_login_error(line):
            hits.append(_ANSI.sub("", line)[-300:])
    return hits


def parse_auth_status(stdout: str) -> bool | None:
    """loggedIn from `claude auth status` JSON; None when it cannot be read."""
    try:
        value = json.loads(stdout).get("loggedIn")
    except (ValueError, AttributeError):  # not JSON / not an object: unknown, the log signal still applies
        return None
    return value if isinstance(value, bool) else None


def claude_cli_logged_in() -> bool | None:
    try:
        out = subprocess.run(["claude", "auth", "status"], capture_output=True, text=True,
                             timeout=AUTH_STATUS_TIMEOUT_S).stdout
    except (OSError, subprocess.TimeoutExpired):  # CLI missing or hung: unknown, the log signal still applies
        return None
    return parse_auth_status(out)


def signed_out_agents(cli_logged_in: bool | None, owner_log_errors: dict) -> dict:
    """Claude Agent -> reason, from the CLI state and each owner's recent log errors."""
    out = {}
    for agent in claude_agents():
        owner = config.AGENTS[agent][0]
        if cli_logged_in is False:
            out[agent] = f"{SIGNED_OUT_NOTE}: `claude auth status` loggedIn=false"
        elif owner_log_errors.get(owner):
            out[agent] = f"{SIGNED_OUT_NOTE}: {owner}'s desktop log: {owner_log_errors[owner][-1][-160:]}"
    return out


def preflight(now: datetime | None = None) -> tuple:
    """(signed_out {agent: reason}, log lines for the report)."""
    now = now or datetime.now(timezone.utc)
    cli = claude_cli_logged_in()
    errors = {o: recent_login_errors(evidence.log_since(p, 0), now) for o, p in config.DESKTOP_LOGS.items()}
    signed_out = signed_out_agents(cli, errors)
    log = [f"claude auth status loggedIn: {'unknown' if cli is None else cli}"]
    log += [f"{o} desktop log: {len(e)} Claude login error(s) in the last "
            f"{int(RECENT_WINDOW.total_seconds() // 60)} min" for o, e in errors.items()]
    log += [f"{a}: {why}" for a, why in signed_out.items()] or ["all Claude Agents' providers usable"]
    return signed_out, log


def desktop_offsets() -> dict:
    return {k: v for k, v in evidence.log_offsets().items() if k.startswith("desktop:")}


def reclassify_silence(result, offsets: dict):
    """A FAIL whose note says a Claude Agent gave no answer, while that Agent's
    owner log showed the login error during the scenario, becomes BLOCKED."""
    if result.status != FAIL or result.kind != "finding" or not any(m in result.note for m in NO_ANSWER_MARKS):
        return result
    by_owner = {}
    for key, line in evidence.grep_logs(offsets, (CLAUDE_LOGIN_REQUIRED, STARTUP_CHECK_FAILED)):
        if is_login_error(line):
            by_owner.setdefault(key.removeprefix("desktop:"), []).append(_ANSI.sub("", line))
    silent = [a for a in claude_agents() if a in result.note and by_owner.get(config.AGENTS[a][0])]
    if not silent:
        return result
    last = by_owner[config.AGENTS[silent[-1]][0]][-1]
    result.status, result.kind = BLOCKED, "setup"
    result.note = f"{SIGNED_OUT_NOTE} during the run ({', '.join(silent)}; owner log: {last[-160:]}). " \
                  f"Was: {result.note}"
    return result
