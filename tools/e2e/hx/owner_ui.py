"""Owner-only steps, driven in the owner's own demo window through the bridge.

Every step is idempotent: it first checks Matrix (as zz-anna) and does
nothing when the owner/Agent is already joined. Every click is preceded by a
check that the element belongs to the e2e room or to a request the runner
caused; anything unexpected stops the flow with StepNotFound."""
import time

from . import config, room
from .bridge import Bridge, StepNotFound, by_id, exact, nearest_below, wait_widget, with_text

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


def open_room(bridge: Bridge, room_name: str):
    rows = wait_widget(bridge, lambda ws: exact(ws, room_name), 20)
    if not rows:
        raise StepNotFound(f"{bridge.owner}: no '{room_name}' in the room list")
    bridge.click(rows[0], settle=2.0)


def join_room(bridge: Bridge, room_id: str, log: list):
    owner_key = f"owner:{bridge.owner}"
    if _status(room_id).get(owner_key) == "join":
        log.append(f"{bridge.owner}: already joined")
        return
    open_room(bridge, config.E2E_ROOM_NAME)
    accept = wait_widget(bridge, lambda ws: by_id(ws, "accept_button"), 20)
    if not accept:
        raise StepNotFound(f"{bridge.owner}: no 'Join Room' button on the e2e room invite")
    bridge.click(accept[0], settle=3.0)
    if not _wait_status(room_id, owner_key, "join", JOIN_WAIT_S):
        raise StepNotFound(f"{bridge.owner}: clicked Join Room but Matrix shows no join in {JOIN_WAIT_S}s")
    log.append(f"{bridge.owner}: joined via Join Room")


def answer_follow_prompts(bridge: Bridge, log: list, timeout_s: float = FOLLOW_WAIT_S):
    """After the join: click "Bring" / "Allow in group rooms" only on prompts
    that name the e2e room."""
    deadline, clicked = time.monotonic() + timeout_s, set()
    while time.monotonic() < deadline:
        ws = bridge.snap(every=True)
        target = _prompt_button(prompt_anchors(ws), ws)
        key = (target.get("i"), tuple(target["r"])) if target else None
        if target and key not in clicked:  # each prompt is answered once
            clicked.add(key)
            bridge.click(target, settle=2.0)
            log.append(f"{bridge.owner}: clicked {target.get('i') or target.get('t')} on an e2e room prompt")
            continue
        time.sleep(2)


def prompt_anchors(widgets: list) -> list:
    """Prompt sentences about the e2e room ("Bring X into e2e room?", "X is
    private … add it to e2e room?"), never the room list row or header."""
    return [w for w in with_text(widgets, config.E2E_ROOM_NAME)
            if any(k in (w.get("t") or "") for k in ("Bring", "private"))]


def _prompt_button(named: list, widgets: list):
    buttons = by_id(widgets, "bring") + [w for w in with_text(widgets, "Allow in group rooms")
                                         if "button" in (w.get("ty") or "").lower() or w.get("i") == "allow_group_rooms"]
    for anchor in named:
        hit = nearest_below(anchor, buttons, fences=tuple(named))
        if hit:
            return hit
    return None


def add_agent(bridge: Bridge, room_id: str, agent: str, log: list):
    key = f"agent:{agent}"
    if _status(room_id).get(key) == "join":
        log.append(f"{agent}: already in the room")
        return
    open_room(bridge, config.E2E_ROOM_NAME)
    for wid in ("new_or_join_button", "plus_add_agent"):
        hit = wait_widget(bridge, lambda ws, w=wid: by_id(ws, w), 10)
        if not hit:
            raise StepNotFound(f"{bridge.owner}: no '{wid}' while adding {agent}")
        bridge.click(hit[0], settle=1.5)
    person = wait_widget(bridge, lambda ws: [w for w in exact(ws, agent) if w.get("i") in ("person_name", "Person")], 15)
    if not person:
        raise StepNotFound(f"{bridge.owner}: {agent} not listed in the invite picker")
    bridge.click(person[0])
    confirm = wait_widget(bridge, lambda ws: by_id(ws, "confirm_button"), 10)
    if not confirm:
        raise StepNotFound(f"{bridge.owner}: no Invite button after picking {agent}")
    bridge.click(confirm[0], settle=3.0)
    _allow_if_private(bridge, agent, log)
    if not _wait_status(room_id, key, "join", BIND_WAIT_S):
        raise StepNotFound(f"{agent}: invited by {bridge.owner} but not joined within {BIND_WAIT_S}s")
    log.append(f"{agent}: added by {bridge.owner}")


def _allow_if_private(bridge: Bridge, agent: str, log: list):
    """The picker asks "Allow in group rooms?" for a private Agent."""
    def find(ws):
        named = [w for w in with_text(ws, agent) if "private" in (w.get("t") or "").lower()
                 and config.E2E_ROOM_NAME in (w.get("t") or "")]
        allow = [w for w in with_text(ws, "Allow in group rooms") if w not in named]
        return [nearest_below(named[0], allow)] if named and allow and nearest_below(named[0], allow) else None
    hit = wait_widget(bridge, find, 8)
    if hit:
        bridge.click(hit[0], settle=3.0)
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


CARD_TEXT_IDS = ("title", "context", "details")  # shared_permission_notice's own labels


def card_anchors(widgets: list, token: str) -> list:
    """The permission card's own text naming `token`; a chat message quoting
    the same token (the runner's request) is never an anchor."""
    return [w for w in with_text(widgets, token) if w.get("i") in CARD_TEXT_IDS]


def visible_texts(owner: str, room_name: str, needles: tuple) -> list:
    """Read-only: open `room_name` in the owner's window and return the
    visible texts that contain any needle (for evidence the runner cannot
    read over Matrix, e.g. the dev room). Sends nothing."""
    bridge = Bridge(owner)
    open_room(bridge, room_name)
    return [text_of for text_of in (w.get("t") or "" for w in bridge.snap(every=True))
            if any(n in text_of for n in needles)]


def deny_card(owner: str, token: str) -> str:
    """Deny the permission card whose own text carries `token` (a string only
    the runner's own request contains) in the e2e room. Never approves."""
    decision = "deny"
    bridge = Bridge(owner)
    open_room(bridge, config.E2E_ROOM_NAME)

    def find(ws):
        anchors = card_anchors(ws, token)
        fences = tuple(w for w in ws if w.get("i") == "title")  # each card starts with its title
        btn = nearest_below(anchors[0], by_id(ws, decision), fences) if anchors else None
        return [btn] if btn else None
    hit = wait_widget(bridge, find, CARD_WAIT_S)
    if not hit:
        raise StepNotFound(f"{owner}: no permission card mentioning {token} with a '{decision}' button")
    bridge.click(hit[0], settle=2.0)
    return f"{owner}: clicked {decision} on the card for {token}"
