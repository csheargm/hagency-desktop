"""Owner-only steps, driven in the owner's own demo window (hx.bridge).

Every step is idempotent: it first checks Matrix (as zz-anna) and does
nothing when the owner/Agent is already in the room (or an invite is still
pending). Each act is chosen from one tree snapshot, and a button only counts
when the text that identifies it (the e2e room, the runner's token) sits in
the same widget container. Ambiguity stops the flow. There is no approve."""
import time

from . import config, room
from .bridge import Bridge, StepNotFound, ends, exact, label, same_card, wait_step

JOIN_WAIT_S = 120  # a beta account joining an alpha room federates first
FOLLOW_WAIT_S = 30
BIND_WAIT_S = 90
CARD_WAIT_S = 120


def _status(room_id: str) -> dict:
    return room.setup_status(room_id)


def _wait_status(room_id: str, key: str, want: str, timeout_s: float) -> bool:
    deadline = time.monotonic() + timeout_s
    while _status(room_id).get(key) != want:
        if time.monotonic() >= deadline:
            return False
        time.sleep(3)
    return True


def _unique(bridge: Bridge, pick, what: str, timeout_s: float = 15) -> dict:
    """Exactly one node from one snapshot, or a named failure."""
    found = wait_step(bridge, pick, timeout_s)
    if not found:
        raise StepNotFound(f"{bridge.owner}: {what} not found")
    if len(found) > 1:
        raise StepNotFound(f"{bridge.owner}: {len(found)} candidates for {what}; not guessing")
    return found[0]


def room_rows(nodes: list, room_name: str) -> list:
    return [n for n in exact(nodes, room_name) if ends(n, "room_name") and ".rooms_list." in f".{n['path']}."]


def open_room(bridge: Bridge, room_name: str):
    bridge.click_at(_unique(bridge, lambda ns: room_rows(ns, room_name), f"room-list row '{room_name}'", 20), 2.0)


def _by_path(*segments):
    return lambda ns: [n for n in ns if ends(n, *segments)]


def join_buttons(nodes: list) -> list:
    """The invite screen's "Join Room" (never the confirmation modal's button)."""
    return [n for n in nodes if ends(n, "accept_button") and ".join_leave_modal." not in f".{n['path']}."
            and label(n) == "Join Room"]


def confirm_buttons(nodes: list) -> list:
    """"Yes" in the "Accept this invite?" modal whose own text names the e2e room."""
    texts = [n for n in nodes if f'"{config.E2E_ROOM_NAME}"' in label(n) and ".join_leave_modal." in f".{n['path']}."]
    return [n for n in nodes if ends(n, "accept_button") and ".join_leave_modal." in f".{n['path']}."
            and label(n) == "Yes" and any(same_card(t, n, 2) for t in texts)]


def join_room(bridge: Bridge, room_id: str, log: list):
    """Join Room opens "Accept this invite? … join "e2e room"?" — answer Yes."""
    key = f"owner:{bridge.owner}"
    if _status(room_id).get(key) == "join":
        log.append(f"{bridge.owner}: already joined")
        return
    if not confirm_buttons(bridge.snapshot()):  # a confirmation left open by an earlier run is answered directly
        open_room(bridge, config.E2E_ROOM_NAME)
        bridge.press(_unique(bridge, join_buttons, "'Join Room' on the e2e room invite"), 2.0)
    bridge.press(_unique(bridge, confirm_buttons, "'Yes' in the e2e room's accept-invite prompt"), 3.0)
    if not _wait_status(room_id, key, "join", JOIN_WAIT_S):
        raise StepNotFound(f"{bridge.owner}: confirmed Join but Matrix shows no join in {JOIN_WAIT_S}s")
    log.append(f"{bridge.owner}: joined via Join Room → Yes")


def prompt_buttons(nodes: list) -> list:
    """(button, prompt text) pairs: Bring / Allow buttons whose own container
    states the e2e room."""
    texts = [n for n in nodes if config.E2E_ROOM_NAME in label(n) and any(k in label(n) for k in ("Bring", "private"))]
    buttons = [n for n in nodes if "press" in n.get("actions", "") and
               (ends(n, "bring") or label(n) == "Allow in group rooms")]
    return [(b, t) for b in buttons for t in texts if same_card(t, b, 2)]


def answer_follow_prompts(bridge: Bridge, log: list, timeout_s: float = FOLLOW_WAIT_S):
    deadline, answered = time.monotonic() + timeout_s, set()
    while time.monotonic() < deadline:
        todo = [(b, t) for b, t in prompt_buttons(bridge.snapshot()) if (b["path"], label(t)) not in answered]
        if todo:
            button, text = todo[0]
            answered.add((button["path"], label(text)))  # a prompt is identified by its text, answered once
            bridge.press(button, settle=2.0)
            log.append(f"{bridge.owner}: pressed {label(button)} on: {label(text)[:80]}")
            continue
        time.sleep(2)


INVITE_TITLE = f"Invite to {config.E2E_ROOM_NAME}"


def _in_invite_modal(n: dict) -> bool:
    return ".invite_modal." in f".{n['path']}."


def invite_modal_open(nodes: list) -> bool:
    return any(_in_invite_modal(n) and label(n) == INVITE_TITLE for n in nodes)


def _close_invite_modal(bridge: Bridge):
    """Close the e2e room's own invite dialog (Okay after an invite, else Cancel)."""
    nodes = bridge.snapshot()
    if not invite_modal_open(nodes):
        return
    for wid in ("okay_button", "cancel_button"):
        hit = [n for n in nodes if _in_invite_modal(n) and ends(n, wid) and "press" in n.get("actions", "")]
        if hit:
            bridge.press(hit[0], settle=1.0)
            return


def add_agent(bridge: Bridge, room_id: str, agent: str, log: list):
    """+ → Add agent to this room → type the Agent's exact MXID → Invite.
    The picker list is not used: it labels every Agent "Your agent", including
    other owners' (product finding), so a pick by name could choose the wrong one."""
    key = f"agent:{agent}"
    state = _status(room_id).get(key)
    if state in ("join", "invite"):  # an invite still binding is not re-sent
        log.append(f"{agent}: already {state} in the room")
        if state == "invite" and not _wait_status(room_id, key, "join", BIND_WAIT_S):
            raise StepNotFound(f"{agent}: invited earlier but not joined within {BIND_WAIT_S}s")
        return
    _close_invite_modal(bridge)
    open_room(bridge, config.E2E_ROOM_NAME)
    bridge.click_at(_unique(bridge, _by_path("new_or_join_button", "icon"), "the + button"), 1.5)
    bridge.press(_unique(bridge, _by_path("plus_add_agent"), "'Add agent to this room'"), 1.5)
    field = _unique(bridge, lambda ns: [n for n in ns if _in_invite_modal(n) and ends(n, "user_id_input")]
                    if invite_modal_open(ns) else [], f"the '{INVITE_TITLE}' field")
    bridge.type_into(field, config.AGENTS[agent][1])
    bridge.press(_unique(bridge, lambda ns: [n for n in ns if _in_invite_modal(n) and ends(n, "confirm_button")
                                             and label(n) == "Invite"], "the Invite button"), 3.0)
    _allow_if_private(bridge, agent, log)
    joined = _wait_status(room_id, key, "join", BIND_WAIT_S)
    _close_invite_modal(bridge)
    if not joined:
        raise StepNotFound(f"{agent}: invited by {bridge.owner} but not joined within {BIND_WAIT_S}s")
    log.append(f"{agent}: added by {bridge.owner}")


def private_allow(nodes: list, agent: str) -> list:
    """The Allow button of the private-Agent prompt naming this agent AND the e2e room."""
    texts = [n for n in nodes if agent in label(n) and "private" in label(n) and config.E2E_ROOM_NAME in label(n)]
    return [n for n in exact(nodes, "Allow in group rooms") if "press" in n.get("actions", "")
            and any(same_card(t, n, 2) for t in texts)]


def _allow_if_private(bridge: Bridge, agent: str, log: list):
    hit = wait_step(bridge, lambda ns: private_allow(ns, agent), 8)
    if hit and len(hit) == 1:
        bridge.press(hit[0], settle=3.0)
        log.append(f"{agent}: allowed in group rooms by {bridge.owner}")


def setup_owner(owner: str, room_id: str) -> list:
    """Join the e2e room and bring every Agent of this owner into it."""
    log: list = []
    bridge = Bridge(owner)
    join_room(bridge, room_id, log)
    answer_follow_prompts(bridge, log)
    for agent, (agent_owner, _) in config.AGENTS.items():
        if agent_owner == owner:
            add_agent(bridge, room_id, agent, log)
    return log


def card_deny(nodes: list, token: str) -> list:
    """Deny buttons of permission cards whose own text names `token`, from one
    snapshot: the room notice or an inbox row. A chat message quoting the token
    is in no card, so it never qualifies."""
    texts = [n for n in nodes if token in label(n)]
    denies = [n for n in nodes if ends(n, "deny") and "press" in n.get("actions", "")]
    return [d for d in denies if any(same_card(t, d, 2) for t in texts)]


def deny_card(owner: str, token: str) -> str:
    """Deny the permission card for the runner's own request. Never approves."""
    bridge = Bridge(owner)
    open_room(bridge, config.E2E_ROOM_NAME)
    deny = _unique(bridge, lambda ns: card_deny(ns, token), f"a permission card naming {token}", CARD_WAIT_S)
    again = _unique(bridge, lambda ns: card_deny(ns, token), f"the card naming {token} (re-check)", 5)
    if again["id"] != deny["id"]:
        raise StepNotFound(f"{owner}: the card naming {token} changed between reads; not pressing")
    bridge.press(again, settle=2.0)
    return f"{owner}: pressed Deny on the card naming {token}"


def visible_texts(owner: str, room_name: str, needles: tuple) -> list:
    """Opt-in evidence: select `room_name` in the owner's window and return the
    visible texts containing any needle. Sends nothing."""
    bridge = Bridge(owner)
    open_room(bridge, room_name)
    return list(dict.fromkeys(label(n) for n in bridge.snapshot() if any(x in label(n) for x in needles)))
