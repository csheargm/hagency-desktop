"""The dedicated "e2e room": created by zz-anna on alpha, personas joined,
owners invited. Agents join only when their owner binds them (desktop)."""
import json

from . import config, personas
from .matrix import MatrixError

ROOM_FILE = config.STATE_DIR / "room.json"


def _saved_room():
    if ROOM_FILE.exists():
        return json.loads(ROOM_FILE.read_text())["room_id"]
    return None


def ensure_room() -> str:
    """Create the room once; later calls re-invite anyone who left."""
    creator = personas.persona("zz-anna")
    room = _saved_room()
    if room is None or room not in creator.joined_rooms():
        owners = [config.mxid(lp, srv) for srv, lp in config.OWNERS.values()]
        room = creator.create_room(config.E2E_ROOM_NAME, invite=owners)
        ROOM_FILE.parent.mkdir(parents=True, exist_ok=True)
        ROOM_FILE.write_text(json.dumps({"room_id": room}))
    members = creator.members(room)
    for name in ("zz-betty", "zz-will"):
        _join_persona(creator, room, name, members)
    for srv, lp in config.OWNERS.values():
        user = config.mxid(lp, srv)
        if members.get(user) not in ("join", "invite"):
            creator.invite(room, user)
    return room


def _join_persona(creator, room, name, members):
    who = personas.persona(name)
    if members.get(who.user_id) == "join":
        return
    if members.get(who.user_id) != "invite":
        creator.invite(room, who.user_id)
    who.join(room, via=[config.SERVERS["alpha"]["name"]])


def setup_status(room: str) -> dict:
    """Who is in the room, from alpha's view: owners and Agents by name."""
    members = personas.persona("zz-anna").members(room)
    status = {}
    for name, (srv, lp) in config.OWNERS.items():
        status[f"owner:{name}"] = members.get(config.mxid(lp, srv), "absent")
    for name, (_, puppet) in config.AGENTS.items():
        status[f"agent:{name}"] = members.get(puppet, "absent")
    return status


def verify_agent_names() -> list:
    """Names that no longer match their puppet (an Agent was recreated)."""
    client = personas.persona("zz-anna")
    wrong = []
    for name, (_, puppet) in config.AGENTS.items():
        try:
            shown = client.display_name(puppet)
        except MatrixError as e:
            shown = f"<{e.status}>"
        if shown != name:
            wrong.append(f"{name} -> {puppet} shows {shown!r}")
    return wrong
