"""The dedicated "e2e room": created by zz-anna on alpha, personas joined,
owners invited. Agents join only when their owner binds them (desktop)."""
import json

from . import config, personas
from .matrix import MatrixError, q

ROOM_FILE = config.STATE_DIR / "room.json"


def _saved_room(creator):
    """The saved room, only if it is still the runner's own e2e room."""
    if not ROOM_FILE.exists():
        return None
    room = json.loads(ROOM_FILE.read_text())["room_id"]
    if room not in creator.joined_rooms():
        return None
    name = creator.state(room, "m.room.name").get("name")
    created_by = creator.state(room, "m.room.create").get("creator")
    if name != config.E2E_ROOM_NAME or created_by != creator.user_id or room == config.DEV_ROOM:
        raise RuntimeError(f"{ROOM_FILE} names {room}, which is not the runner's e2e room")
    return room


def ensure_room() -> str:
    """Create the room once; later calls re-invite anyone who left."""
    creator = personas.persona("zz-anna")
    room = _saved_room(creator)
    if room is None:
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
    set_repo_linked(room, True)
    return room


def linked_repos(room: str) -> list:
    """The room's hand-linked repositories (empty when the event is absent)."""
    try:
        return list(personas.persona("zz-anna").state(room, config.ROOM_REPOS_STATE).get("repos", []))
    except MatrixError as e:
        if e.status == 404:
            return []
        raise


def set_repo_linked(room: str, on: bool) -> bool:
    """Link (or unlink) the sandbox repo in the room's repo set as zz-anna,
    the room's creator. Returns whether anything changed; idempotent."""
    repos = linked_repos(room)
    present = any(r.lower() == config.SANDBOX_REPO.lower() for r in repos)
    if present == on:
        return False
    kept = [r for r in repos if r.lower() != config.SANDBOX_REPO.lower()]
    content = {"repos": kept + [config.SANDBOX_REPO] if on else kept}
    personas.persona("zz-anna").call("PUT", f"/rooms/{q(room)}/state/{q(config.ROOM_REPOS_STATE)}/", content)
    return True


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
