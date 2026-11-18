"""Scenario plumbing: results, waiting, messages and room observation.

R14: a timeout or an empty observation is FAIL, never PASS. A scenario whose
precondition (setup) is missing is BLOCKED, which also never counts as PASS."""
import time
from dataclasses import dataclass, field

from . import config, personas
from .matrix import MatrixError

PASS, FAIL, BLOCKED, OPT_IN = "PASS", "FAIL", "BLOCKED", "OPT-IN (not run)"
POLL_S = 3


@dataclass
class Result:
    name: str
    status: str = FAIL
    seconds: float = 0.0
    evidence: list = field(default_factory=list)
    note: str = ""
    kind: str = ""  # "finding" (product behaviour) or "runner" (harness error)


@dataclass
class Ctx:
    room: str
    setup: dict
    run_id: str
    created_issues: list = field(default_factory=list)
    created_branches: list = field(default_factory=list)
    loop_states: dict = field(default_factory=dict)
    setup_log: list = field(default_factory=list)
    read_dev_room: bool = False  # opt-in: navigates Betty's window to the dev room

    def agent_in_room(self, name: str) -> bool:
        return self.setup.get(f"agent:{name}") == "join"


def wait_for(probe, timeout_s: float, poll_s: float = POLL_S):
    """Call probe() until it returns something truthy or the time runs out."""
    deadline = time.monotonic() + timeout_s
    while True:
        value = probe()
        if value:
            return value
        if time.monotonic() >= deadline:
            return None
        time.sleep(poll_s)


def mention_content(text: str, agents: list, thread_root: str = "") -> dict:
    """What the desktop composer sends for "@agent text": a pill per Agent
    plus m.mentions, which is what the agent service routes on."""
    puppets = [config.AGENTS[a][1] for a in agents]
    pills = " ".join(f'<a href="https://matrix.to/#/{p}">{a}</a>' for a, p in zip(agents, puppets))
    content = {"msgtype": "m.text", "body": f"{' '.join(agents)}: {text}",
               "format": "org.matrix.custom.html", "formatted_body": f"{pills}: {text}",
               "m.mentions": {"user_ids": puppets}}
    return _threaded(content, thread_root)


def plain_content(text: str, thread_root: str = "", room_mention=False) -> dict:
    content = {"msgtype": "m.text", "body": text, "m.mentions": {}}
    if room_mention:
        content["m.mentions"] = {"room": True}
    return _threaded(content, thread_root)


def _threaded(content: dict, thread_root: str) -> dict:
    if thread_root:
        content["m.relates_to"] = {"rel_type": "m.thread", "event_id": thread_root,
                                   "is_falling_back": True, "m.in_reply_to": {"event_id": thread_root}}
    return content


def agent_messages(viewer: str, room: str, agent: str, since_ms: int) -> list:
    """Messages the Agent sent since `since_ms`, as one persona's server sees them."""
    puppet = config.AGENTS[agent][1]
    events = personas.persona(viewer).messages_after(room, since_ms)
    return [e for e in events if e["sender"] == puppet and e["type"] == "m.room.message"
            and e["content"].get("m.relates_to", {}).get("rel_type") != "m.replace"]


def substantive(events: list) -> list:
    """Agent messages that are answers, not "working on it" status lines."""
    return [e for e in events if len(e["content"].get("body", "")) >= 2]


def first_reply(viewer: str, room: str, agent: str, since_ms: int, timeout_s: float):
    found = wait_for(lambda: substantive(agent_messages(viewer, room, agent, since_ms)), timeout_s)
    return found[0] if found else None


def reply_with(viewer: str, room: str, agent: str, since_ms: int, token: str, timeout_s: float):
    """The Agent's first message containing `token`: progress notices and
    unrelated messages never count as the answer."""
    found = wait_for(lambda: [e for e in agent_messages(viewer, room, agent, since_ms)
                              if token in e["content"].get("body", "")], timeout_s)
    return found[0] if found else None


def server_ts(viewer: str, room: str, event_id: str) -> int:
    """origin_server_ts of an event: latency is measured on the server clock."""
    return personas.persona(viewer).event(room, event_id)["origin_server_ts"]


def seen_on(viewer: str, room: str, event_id: str) -> bool:
    try:
        personas.persona(viewer).event(room, event_id)
        return True
    except MatrixError as e:
        if e.status in (403, 404):  # not federated yet: the caller's wait times out to FAIL
            return False
        raise


def blocked(name: str, ctx: Ctx, agents: list) -> Result | None:
    missing = [a for a in agents if not ctx.agent_in_room(a)]
    if not missing:
        return None
    owners = sorted({config.AGENTS[a][0] for a in missing})
    return Result(name, BLOCKED, note=f"not in e2e room: {', '.join(missing)} — one-time setup: "
                  f"{' and '.join(owners)} not set up — the runner does it through each window's bridge "
                  f"(see the report's 'Owner UI setup' lines and SETUP.md)",
                  kind="setup")


def now_ms() -> int:
    return int(time.time() * 1000)
