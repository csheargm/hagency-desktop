"""Owner-only steps, driven in the owner's own demo window (hx.bridge).

Every step is idempotent: it first checks Matrix (as zz-anna) and does
nothing when the owner/Agent is already joined. Buttons are found by their
widget path and pressed by node id; a button only counts when the text that
identifies it (the e2e room, the runner's token) sits in the same container.
Anything unexpected stops the flow with StepNotFound. There is no approve."""
import time

from . import config, room
from .bridge import Bridge, StepNotFound, ends, exact, label, same_card, wait_nodes

JOIN_WAIT_S = 60
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


def _one(bridge: Bridge, query: str, accept, what: str, timeout_s: float = 15) -> dict:
    found = wait_nodes(bridge, lambda b: [n for n in b.nodes(query) if accept(n)], timeout_s)
    if not found:
        raise StepNotFound(f"{bridge.owner}: {what} not found")
    return found[0]


def open_room(bridge: Bridge, room_name: str):
    row = _one(bridge, f'name="{room_name}"', lambda n: ends(n, "room_name") and label(n) == room_name,
               f"room-list row '{room_name}'", 20)
    bridge.click_at(row, settle=2.0)


def join_room(bridge: Bridge, room_id: str, log: list):
    key = f"owner:{bridge.owner}"
    if _status(room_id).get(key) == "join":
        log.append(f"{bridge.owner}: already joined")
        return
    open_room(bridge, config.E2E_ROOM_NAME)
    bridge.press(_one(bridge, "path=accept_button", lambda n: ends(n, "accept_button"), "'Join Room' on the e2e room invite"), 3.0)
    if not _wait_status(room_id, key, "join", JOIN_WAIT_S):
        raise StepNotFound(f"{bridge.owner}: pressed Join Room but Matrix shows no join in {JOIN_WAIT_S}s")
    log.append(f"{bridge.owner}: joined via Join Room")


def prompt_buttons(nodes: list) -> list:
    """Bring / Allow buttons whose own container states the e2e room."""
    texts = [n for n in nodes if config.E2E_ROOM_NAME in label(n) and any(k in label(n) for k in ("Bring", "private"))]
    buttons = [n for n in nodes if "press" in n.get("actions", "") and
               (ends(n, "bring") or label(n) == "Allow in group rooms")]
    return [b for b in buttons if any(same_card(t, b, 2) for t in texts)]


def answer_follow_prompts(bridge: Bridge, log: list, timeout_s: float = FOLLOW_WAIT_S):
    deadline, pressed = time.monotonic() + timeout_s, set()
    while time.monotonic() < deadline:
        todo = [b for b in prompt_buttons(bridge.nodes("action=press") + bridge.nodes(config.E2E_ROOM_NAME))
                if b["path"] not in pressed]
        if todo:
            pressed.add(todo[0]["path"])  # each prompt is answered once
            bridge.press(todo[0], settle=2.0)
            log.append(f"{bridge.owner}: pressed {label(todo[0])} on an e2e room prompt")
            continue
        time.sleep(2)


def add_agent(bridge: Bridge, room_id: str, agent: str, log: list):
    key = f"agent:{agent}"
    if _status(room_id).get(key) == "join":
        log.append(f"{agent}: already in the room")
        return
    open_room(bridge, config.E2E_ROOM_NAME)
    plus = _one(bridge, "path=new_or_join_button.icon", lambda n: ends(n, "new_or_join_button", "icon"), "the + button")
    bridge.click_at(plus, settle=1.5)
    bridge.press(_one(bridge, "path=plus_add_agent", lambda n: ends(n, "plus_add_agent"), "'Add agent to this room'"), 1.5)
    person = _one(bridge, f'name="{agent}"', lambda n: "people_list" in n["path"] and label(n) == agent,
                  f"{agent} in the invite picker")
    bridge.click_at(person)
    bridge.press(_one(bridge, "path=confirm_button", lambda n: ends(n, "confirm_button"), "the Invite button"), 3.0)
    _allow_if_private(bridge, agent, log)
    if not _wait_status(room_id, key, "join", BIND_WAIT_S):
        raise StepNotFound(f"{agent}: invited by {bridge.owner} but not joined within {BIND_WAIT_S}s")
    log.append(f"{agent}: added by {bridge.owner}")


def _allow_if_private(bridge: Bridge, agent: str, log: list):
    """The picker asks "Allow in group rooms?" for a private Agent."""
    def find(b):
        nodes = b.nodes("Allow") + b.nodes("private")
        texts = [n for n in nodes if agent in label(n) and "private" in label(n)]
        return [n for n in exact(nodes, "Allow in group rooms") if "press" in n.get("actions", "")
                and any(same_card(t, n, 2) for t in texts)]
    hit = wait_nodes(bridge, find, 8)
    if hit:
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
    """Deny buttons of permission cards whose own text names `token`: the room
    notice (shared_permission_notice) or an inbox row (rowN). A chat message
    quoting the token is in no card, so it never qualifies."""
    texts = [n for n in nodes if token in label(n)]
    denies = [n for n in nodes if ends(n, "deny") and "press" in n.get("actions", "")]
    return [d for d in denies if any(same_card(t, d, 2) for t in texts)]


def deny_card(owner: str, token: str) -> str:
    """Deny the permission card for the runner's own request. Never approves."""
    bridge = Bridge(owner)
    open_room(bridge, config.E2E_ROOM_NAME)
    hit = wait_nodes(bridge, lambda b: card_deny(b.nodes(token) + b.nodes("path=deny"), token), CARD_WAIT_S)
    if not hit:
        raise StepNotFound(f"{owner}: no permission card naming {token} with a Deny button")
    bridge.press(hit[0], settle=2.0)
    return f"{owner}: pressed Deny on the card for {token}"


def visible_texts(owner: str, room_name: str, needles: tuple) -> list:
    """Read-only apart from selecting the room: open `room_name` in the owner's
    window and return the visible texts that contain any needle. Sends nothing."""
    bridge = Bridge(owner)
    open_room(bridge, room_name)
    found = []
    for needle in needles:
        found += [label(n) for n in bridge.nodes(needle)]
    return list(dict.fromkeys(found))
