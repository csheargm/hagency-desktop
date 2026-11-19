"""Is each Agent's model provider usable? A signed-out Claude Code login makes
every Claude Agent silent; that is a missing precondition (BLOCKED), not a
product finding (FAIL), and never a pass (R14).

Two signals, either one blocks:
- `claude auth status` (JSON; only `loggedIn` is read, nothing is printed),
  run under the CLAUDE_CONFIG_DIR the owner's desktop has (its process env,
  else a literal one in int-demo-up.sh; unset when none);
- the owner's desktop log: the agent runtime's CLAUDE_LOGIN_REQUIRED profile
  error (hagency-agent-local runtime.rs) in the last few minutes."""
import json
import os
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
LOGIN_PHRASES = ("login required", "not logged in")
NO_ANSWER_MARKS = ("(nothing)", "no answer")
_ANSI = re.compile(r"\x1b\[[0-9;]*m")
_CONFIG_DIR_ENV = re.compile(r"(?:^|\s)CLAUDE_CONFIG_DIR=(.+?)(?=\s+[A-Za-z_][A-Za-z0-9_]*=|$)")
_CONFIG_DIR_SCRIPT = re.compile(r"--env\s+CLAUDE_CONFIG_DIR=(?:\"([^\"]*)\"|'([^']*)'|(\S+))")
UNKNOWN_DIR = "<unknown>"  # the desktop's config dir cannot be told: skip the CLI check
_ITEM_END = re.compile(r"(?<=\.)\s+")
_TS = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.\d+)?Z")


def claude_agents() -> list:
    return [a for a in config.AGENTS if config.AGENT_PROVIDERS[a] == "claude"]


def is_login_error(line: str) -> bool:
    """A desktop-log line saying the owner's Claude login is missing."""
    text = _ANSI.sub("", line)
    if CLAUDE_LOGIN_REQUIRED in text:
        return True
    lowered = text.lower()  # a start-up failure counts only when it says the Claude login is missing
    return STARTUP_CHECK_FAILED in text and "claude" in lowered and any(p in lowered for p in LOGIN_PHRASES)


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
            hits.append(log_excerpt(line))
    return hits


def log_excerpt(line: str) -> str:
    """Timestamp + the matched marker phrase only: the rest of a log line may
    carry paths or secrets and never reaches the report."""
    ts = line_time(line)
    text = _ANSI.sub("", line)
    marker = CLAUDE_LOGIN_REQUIRED if CLAUDE_LOGIN_REQUIRED in text else STARTUP_CHECK_FAILED
    return f"{ts.strftime('%Y-%m-%dT%H:%M:%SZ') if ts else 'undated'} {marker}"


def describe_dir(config_dir: str | None) -> str:
    """How a config dir is named in the report: never its raw path."""
    if config_dir == UNKNOWN_DIR:
        return "unknown config"
    return "dedicated config" if config_dir else "default config"


def parse_auth_status(stdout: str) -> bool | None:
    """loggedIn from `claude auth status` JSON; None when it cannot be read."""
    try:
        value = json.loads(stdout).get("loggedIn")
    except (ValueError, AttributeError):  # not JSON / not an object: unknown, the log signal still applies
        return None
    return value if isinstance(value, bool) else None


def env_config_dir(ps_env_output: str, argv: str) -> str | None:
    """CLAUDE_CONFIG_DIR from `ps -E` output: only the environment after the
    process's own argv is searched; a value runs to the next NAME= token, so a
    path with spaces survives. Nothing else from the environment is kept."""
    if not argv or not ps_env_output.startswith(argv):
        return UNKNOWN_DIR
    m = _CONFIG_DIR_ENV.search(ps_env_output[len(argv):].rstrip("\n"))
    return m.group(1) if m else None


def _launch_commands(script_text: str) -> list:
    """Shell commands of the script with backslash continuations joined."""
    return [c for c in re.sub(r"\\\n", " ", script_text).splitlines() if c.strip()]


def script_config_dir(script_text: str, owner: str) -> str | None:
    """The literal `--env CLAUDE_CONFIG_DIR=...` of the launch command that
    names this owner's desktop (fresh-<owner>); UNKNOWN_DIR when the script
    sets one it cannot attribute or resolve ($VAR), None when it sets none."""
    hits = [c for c in _launch_commands(script_text) if _CONFIG_DIR_SCRIPT.search(c)]
    if not hits:
        return None
    mine = [c for c in hits if re.search(rf"fresh-{re.escape(owner)}\b", c)]
    if len(mine) != 1:
        return UNKNOWN_DIR
    m = _CONFIG_DIR_SCRIPT.search(mine[0])
    value = next(g for g in m.groups() if g is not None)
    return value if "$" not in value else UNKNOWN_DIR


def _ps(pid: str, with_env: bool) -> str:
    flags = "-wwE" if with_env else "-ww"
    return subprocess.run(["ps", flags, "-p", pid, "-o", "command="], capture_output=True, text=True,
                          timeout=AUTH_STATUS_TIMEOUT_S).stdout


def desktop_config_dir(owner: str) -> str | None:
    """The CLAUDE_CONFIG_DIR the owner's running desktop has (its process env),
    else this owner's literal one in int-demo-up.sh; None = CLI default,
    UNKNOWN_DIR = cannot tell (the CLI check is then skipped)."""
    try:
        pid = config.DESKTOP_PIDS[owner].read_text().strip()
        if pid.isdigit():
            found = env_config_dir(_ps(pid, True), _ps(pid, False).rstrip("\n"))
            if found != UNKNOWN_DIR:
                return found
    except (OSError, subprocess.TimeoutExpired):  # no pid file / ps failed: fall back to the launch script
        pass
    try:
        return script_config_dir(config.DEMO_UP_SCRIPT.read_text(), owner)
    except OSError:  # no launch script: the CLI default login is the one the desktop inherits
        return None


def claude_cli_logged_in(config_dir: str | None = None) -> bool | None:
    """`claude auth status` under `config_dir` (CLAUDE_CONFIG_DIR unset when None)."""
    env = {k: v for k, v in os.environ.items() if k != "CLAUDE_CONFIG_DIR"}
    if config_dir:
        env["CLAUDE_CONFIG_DIR"] = config_dir
    try:
        out = subprocess.run(["claude", "auth", "status"], capture_output=True, text=True, env=env,
                             timeout=AUTH_STATUS_TIMEOUT_S).stdout
    except (OSError, subprocess.TimeoutExpired):  # CLI missing or hung: unknown, the log signal still applies
        return None
    return parse_auth_status(out)


def owner_cli_status(dirs: dict, probe=None) -> dict:
    """owner -> loggedIn under that owner's config dir (one probe per distinct dir)."""
    probe, cache = probe or claude_cli_logged_in, {UNKNOWN_DIR: None}
    for d in dict.fromkeys(dirs.values()):
        if d not in cache:
            cache[d] = probe(d)
    return {o: cache[d] for o, d in dirs.items()}


def signed_out_agents(cli_by_owner: dict, owner_log_errors: dict, dirs: dict | None = None) -> dict:
    """Claude Agent -> reason. A readable CLI status decides (a fresh loggedIn=true
    beats older log lines); the log errors decide only when it is unknown."""
    out, dirs = {}, dirs or {}
    for agent in claude_agents():
        owner = config.AGENTS[agent][0]
        if cli_by_owner.get(owner) is False:
            out[agent] = f"{SIGNED_OUT_NOTE}: `claude auth status` ({describe_dir(dirs.get(owner))}) loggedIn=false"
        elif cli_by_owner.get(owner) is None and owner_log_errors.get(owner):
            out[agent] = f"{SIGNED_OUT_NOTE}: {owner}'s desktop log: {owner_log_errors[owner][-1]}"
    return out


def preflight(now: datetime | None = None) -> tuple:
    """(signed_out {agent: reason}, log lines for the report)."""
    now = now or datetime.now(timezone.utc)
    dirs = {o: desktop_config_dir(o) for o in config.DESKTOP_LOGS}
    cli = owner_cli_status(dirs)
    errors = {o: recent_login_errors(evidence.log_since(p, 0), now) for o, p in config.DESKTOP_LOGS.items()}
    signed_out = signed_out_agents(cli, errors, dirs)
    log = [f"{o}: claude auth status ({describe_dir(dirs[o])}) loggedIn: "
           f"{'unknown' if cli[o] is None else cli[o]}" for o in dirs]
    log += [f"{o} desktop log: {len(e)} Claude login error(s) in the last "
            f"{int(RECENT_WINDOW.total_seconds() // 60)} min" for o, e in errors.items()]
    log += [f"{a}: {why}" for a, why in signed_out.items()] or ["all Claude Agents' providers usable"]
    return signed_out, log


def desktop_offsets() -> dict:
    return {k: v for k, v in evidence.log_offsets().items() if k.startswith("desktop:")}


def failure_items(note: str) -> list:
    """The failures a scenario recorded: its note is a run of ". "-ended items."""
    return [x.strip() for x in _ITEM_END.split(note.strip()) if x.strip(" .")]


def _silent_agent(item: str, signed_out_owners: set) -> str | None:
    """The signed-out Claude Agent this failure item says gave no answer."""
    if not any(m in item for m in NO_ANSWER_MARKS):
        return None
    return next((a for a in claude_agents() if a in item and config.AGENTS[a][0] in signed_out_owners), None)


def reclassify_silence(result, offsets: dict):
    """A FAIL becomes BLOCKED only when every failure it recorded is a no-answer
    from a Claude Agent whose owner log showed the login error during the
    scenario; any other failure (wrong answer, an approval raised) keeps it FAIL."""
    items = failure_items(result.note)
    if result.status != FAIL or result.kind != "finding" or not items:
        return result
    by_owner = {}
    for key, (path, offset) in offsets.items():  # whole lines: the excerpt needs the leading timestamp
        for line in evidence.log_since(path, offset).splitlines():
            if is_login_error(line):
                by_owner.setdefault(key.removeprefix("desktop:"), []).append(log_excerpt(line))
    found = [_silent_agent(item, set(by_owner)) for item in items]
    if not all(found):
        return result
    silent = list(dict.fromkeys(found))
    last = by_owner[config.AGENTS[silent[-1]][0]][-1]
    result.status, result.kind = BLOCKED, "setup"
    result.note = f"{SIGNED_OUT_NOTE} during the run ({', '.join(silent)}; owner log: {last}). " \
                  f"Was: {result.note}"
    return result
