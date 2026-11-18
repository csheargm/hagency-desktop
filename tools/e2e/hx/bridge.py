"""Drive a demo window through its makepad-remote endpoint (see remote_wire).

Reads take the whole semantic tree in one request (`ui json=1`): nodes with
role, name, text, actions, widget path and bounds, so every match is made
within one snapshot. Buttons are pressed by node id (`ui act press`); only
nodes that are not buttons (a room-list row, the + icon) get a pointer click.
Each step captures the person's input counter before it reads the tree and
sends it with the act (`if_user_seq`), so the window refuses the act if the
person touched it meanwhile. Only an allowlist of commands is ever sent."""
import json
import subprocess
import time

from . import config
from .remote_wire import Session, WireError, socket_path

WINDOW_LABELS = {"anna": "fresh-anna", "betty": "fresh-betty"}
ALLOWED = ("status", "ui", "ui act press", "ui type", "click")


class BridgeDown(RuntimeError):
    pass


class StepNotFound(RuntimeError):
    """A UI element the flow needs is not on screen, or not safely identifiable."""


def window_pid(owner: str) -> int:
    pid_file = config.RUN / WINDOW_LABELS[owner] / "desktop.pid"
    try:
        pid = int(pid_file.read_text().strip())
    except (OSError, ValueError) as e:
        raise BridgeDown(f"bridge not up yet: {pid_file} has no pid ({e})") from e
    cmd = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True).stdout
    expected = f"/Hagency-{WINDOW_LABELS[owner]}.app/Contents/MacOS/"
    if pid <= 1 or expected not in cmd:
        raise BridgeDown(f"bridge not up yet: pid {pid} is not {owner}'s window")
    return pid


class Bridge:
    def __init__(self, owner: str, session_factory=None):
        self.owner, self.seq = owner, None
        try:
            path = socket_path(window_pid(owner))
            if not path.exists():
                raise BridgeDown(f"bridge not up yet: {owner}'s window has no endpoint socket {path}")
            self.session = (session_factory or Session)(path)
        except (WireError, OSError) as e:
            raise BridgeDown(f"bridge not up yet: {owner}'s window endpoint: {e}") from e

    def cmd(self, words: str, **params) -> list:
        """One allowlisted `cmd` request in the JSON request form."""
        if words not in ALLOWED or "cmd" in params:
            raise ValueError(f"`{words}` is not something the runner may send to a demo window")
        try:
            return self.session.request(json.dumps({"cmd": words, **params}))
        except (WireError, OSError, ValueError) as e:
            raise BridgeDown(f"{self.owner}'s window stopped answering `{words}`: {e}") from e

    def snapshot(self) -> list:
        """Start a step: capture the input counter, then read the whole tree once."""
        self.seq = self.user_seq()
        out = []
        for row in self.cmd("ui", json=1, full=1):
            for tree in row.get("trees", [row] if "nodes" in row else []):
                out += [{**n, "window": tree.get("window", "1:0")} for n in tree.get("nodes", [])]
        return [n for n in out if visible(n)]

    def user_seq(self) -> int:
        """The person's input counter; refuses to act while they are using the window."""
        activity = self.cmd("status")[0].get("activity", {})
        if activity.get("user_active"):
            raise StepNotFound(f"{self.owner}'s window is in use by a person; not acting")
        return int(activity.get("user_seq") or 0)

    def _seq(self) -> int:
        if self.seq is None:
            raise StepNotFound("act without a snapshot: refusing to act on unread state")
        return self.seq

    def press(self, node: dict, settle: float = 1.0):
        """Press a button by node id. A node without a press action fails closed."""
        if "press" not in node.get("actions", ""):
            raise StepNotFound(f"{label(node)!r} ({node.get('path', '')[-60:]}) offers no press action")
        self.cmd("ui act press", node=node["id"], wait=1, if_user_seq=self._seq(), json=1)
        self.seq = None
        time.sleep(settle)

    def click_at(self, node: dict, settle: float = 1.0):
        """Pointer click for a non-button node (row, icon); never for a button."""
        if node.get("role") == "button":
            raise StepNotFound(f"{label(node)!r} is a button: press it, never click its coordinates")
        x, y, w, h = node["bounds"]
        win = node.get("window", "1:0").split(":")[1]
        self.cmd("click", x=round(x + w / 2), y=round(y + h / 2), w=int(win), wait=1, if_user_seq=self._seq())
        self.seq = None
        time.sleep(settle)


    def type_into(self, node: dict, text: str, settle: float = 0.5):
        """Replace a text field's content (only text fields)."""
        if node.get("role") != "text_field":
            raise StepNotFound(f"{label(node)!r} is not a text field")
        self.cmd("ui type", node=node["id"], t=text, if_user_seq=self._seq(), json=1)
        self.seq = None
        time.sleep(settle)


def visible(n: dict) -> bool:
    b = n.get("bounds") or [0, 0, 0, 0]
    return b[2] > 0 and b[3] > 0 and "hidden" not in n.get("states", "") and "offscreen" not in n.get("states", "")


def label(n: dict) -> str:
    return (n.get("name") or n.get("text") or n.get("value") or "").strip()


def ends(n: dict, *segments: str) -> bool:
    return n.get("path", "").split(".")[-len(segments):] == list(segments)


def exact(nodes: list, text: str) -> list:
    return [n for n in nodes if label(n).lower() == text.lower()]


def same_card(anchor: dict, button: dict, depth: int) -> bool:
    """Whether `button` sits in the container that also holds `anchor`: the
    button's path minus its last `depth` segments is a prefix of the anchor's,
    in the same window."""
    container = button["path"].split(".")[:-depth]
    return bool(container) and anchor.get("window") == button.get("window") and \
        anchor["path"].split(".")[:len(container)] == container


def wait_step(bridge: Bridge, find, timeout_s: float, poll_s: float = 1.5):
    """Snapshot until `find(nodes)` returns something; the bridge keeps that
    snapshot's input counter for the act that follows. None at the deadline."""
    deadline = time.monotonic() + timeout_s
    while True:
        found = find(bridge.snapshot())
        if found:
            return found
        if time.monotonic() >= deadline:
            return None
        time.sleep(poll_s)
