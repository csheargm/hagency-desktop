"""Drive a demo window through its makepad-remote endpoint (see remote_wire).

Reads use the semantic tree (`ui find ... json=1`): nodes with role, name,
text, actions, widget path and bounds. Buttons are pressed by node id
(`ui act node=ID press`); only a node without a press action (a room-list
row) gets a pointer click at its bounds. Every act carries `if_user_seq`, so
the window refuses it once the person has touched the window, and the client
refuses the destructive commands (quit, close, gq) outright."""
import json
import subprocess
import time

from . import config
from .remote_wire import Session, WireError, socket_path

WINDOW_LABELS = {"anna": "fresh-anna", "betty": "fresh-betty"}
REFUSED_WORDS = ("quit", "close", "gq", "input", "cap", "design", "tweak", "window", "resize")


class BridgeDown(RuntimeError):
    pass


class StepNotFound(RuntimeError):
    """A UI element the flow needs is not on screen."""


def window_pid(owner: str) -> int:
    pid_file = config.RUN / WINDOW_LABELS[owner] / "desktop.pid"
    if not pid_file.exists():
        raise BridgeDown(f"bridge not up yet: no {pid_file}")
    pid = int(pid_file.read_text().strip())
    cmd = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True).stdout
    if pid <= 1 or WINDOW_LABELS[owner] not in cmd:
        raise BridgeDown(f"bridge not up yet: pid {pid} is not {owner}'s window")
    return pid


class Bridge:
    def __init__(self, owner: str, session_factory=None):
        self.owner = owner
        try:
            path = socket_path(window_pid(owner))
            if not path.exists():
                raise BridgeDown(f"bridge not up yet: {owner}'s window has no endpoint socket {path}")
            self.session = (session_factory or Session)(path)
        except (WireError, OSError) as e:
            raise BridgeDown(f"bridge not up yet: {owner}'s window endpoint: {e}") from e

    def cmd(self, words: str, **params) -> list:
        """One `cmd` request in the JSON request form (values may hold spaces)."""
        if words.split()[0] in REFUSED_WORDS:
            raise ValueError(f"`{words.split()[0]}` is not something the runner may do to a demo window")
        try:
            return self.session.request(json.dumps({"cmd": words, **params}))
        except (WireError, OSError, ValueError) as e:
            raise BridgeDown(f"{self.owner}'s window stopped answering `{words}`: {e}") from e

    def nodes(self, query: str) -> list:
        """Visible nodes matching a ui query, each with its `window` key."""
        rows = self.cmd("ui find", q=f"{query} state=visible", json=1)
        out = []
        for row in rows:
            for tree in row.get("trees", []):
                out += [{**n, "window": tree["window"]} for n in tree.get("nodes", [])]
        return out

    def user_seq(self) -> int:
        """The person's input counter; refuses to act while they are using the window."""
        activity = self.cmd("status")[0].get("activity", {})
        if activity.get("user_active"):
            raise StepNotFound(f"{self.owner}'s window is in use by a person; not acting")
        return int(activity.get("user_seq") or 0)

    def press(self, node: dict, settle: float = 1.0):
        if "press" not in node.get("actions", ""):
            return self.click_at(node, settle)
        self.cmd("ui act press", node=node["id"], wait=1, if_user_seq=self.user_seq(), json=1)
        time.sleep(settle)

    def click_at(self, node: dict, settle: float = 1.0):
        x, y, w, h = node["bounds"]
        win = node.get("window", "1:0").split(":")[1]
        self.cmd("click", x=round(x + w / 2), y=round(y + h / 2), w=int(win), wait=1, if_user_seq=self.user_seq())
        time.sleep(settle)


def label(n: dict) -> str:
    return (n.get("name") or n.get("text") or n.get("value") or "").strip()


def ends(n: dict, *segments: str) -> bool:
    return n.get("path", "").split(".")[-len(segments):] == list(segments)


def exact(nodes: list, text: str) -> list:
    return [n for n in nodes if label(n).lower() == text.lower()]


def same_card(anchor: dict, button: dict, depth: int) -> bool:
    """Whether `button` sits in the container that also holds `anchor`: the
    button's path minus its last `depth` segments is a prefix of the anchor's."""
    container = button["path"].split(".")[:-depth]
    return bool(container) and anchor["path"].split(".")[:len(container)] == container


def wait_nodes(bridge: Bridge, find, timeout_s: float, poll_s: float = 1.5):
    """Poll until `find(bridge)` returns something truthy, or None at the deadline."""
    deadline = time.monotonic() + timeout_s
    while True:
        found = find(bridge)
        if found:
            return found
        if time.monotonic() >= deadline:
            return None
        time.sleep(poll_s)
