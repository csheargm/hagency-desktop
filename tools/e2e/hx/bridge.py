"""Client for Makepad's localhost control bridge (MAKEPAD_REMOTE=<port>).

Same surface as tools/agentchat-probe/probe_approval.py: /snap lists widget
rects ({"i": id, "ty": type, "r": [x, y, w, h], "t": text}), /click injects a
real click. Never calls /gq or /quit: these are the live demo windows."""
import json
import socket
import time
import urllib.parse
import urllib.request

BRIDGE_PORTS = {"anna": 8771, "betty": 8772}
TIMEOUT_S = 20
FORBIDDEN_ROUTES = ("/gq", "/quit", "/close")


class BridgeDown(RuntimeError):
    pass


class StepNotFound(RuntimeError):
    """A UI element the flow needs is not on screen."""


def port_open(port: int) -> bool:
    with socket.socket() as s:
        s.settimeout(1)
        return s.connect_ex(("127.0.0.1", port)) == 0


class Bridge:
    def __init__(self, owner: str):
        self.owner, self.port = owner, BRIDGE_PORTS[owner]
        if not port_open(self.port):
            raise BridgeDown(f"bridge not up yet: {owner}'s window has no listener on 127.0.0.1:{self.port} "
                             "(needs a window restart with MAKEPAD_REMOTE)")

    def get(self, path: str) -> dict:
        if path.split("?")[0] in FORBIDDEN_ROUTES:
            raise ValueError(f"{path} would close a demo window")
        with urllib.request.urlopen(f"http://127.0.0.1:{self.port}{path}", timeout=TIMEOUT_S) as r:
            return json.loads(r.read().decode() or "{}")

    def snap(self, query: str = "", every: bool = False) -> list:
        params = {k: v for k, v in (("q", query), ("all", "1" if every else "")) if v}
        return self.get("/snap?" + urllib.parse.urlencode(params)).get("s", [])

    def click(self, widget: dict, settle: float = 1.0):
        x, y = center(widget)
        self.get(f"/click?x={x:.0f}&y={y:.0f}&wait=1")
        time.sleep(settle)

    def grab(self) -> str:
        """Path or payload of a window capture, for evidence."""
        return str(self.get("/g").get("png", ""))[:200]


def center(w: dict) -> tuple:
    x, y, width, height = w["r"]
    return x + width / 2.0, y + height / 2.0


def text(w: dict) -> str:
    return (w.get("t") or "").strip()


def by_id(widgets: list, wid: str) -> list:
    """Widgets whose id is exactly `wid`, largest first."""
    hits = [w for w in widgets if w.get("i") == wid]
    return sorted(hits, key=lambda w: -w["r"][2] * w["r"][3])


def with_text(widgets: list, needle: str) -> list:
    """Widgets whose text contains `needle` (case-insensitive), largest first."""
    hits = [w for w in widgets if needle.lower() in text(w).lower()]
    return sorted(hits, key=lambda w: -w["r"][2] * w["r"][3])


def nearest_below(anchor: dict, candidates: list) -> dict | None:
    """The candidate closest under (or level with) `anchor`: the button that
    belongs to the same card as the text that identified it."""
    ax, ay = center(anchor)
    below = [c for c in candidates if center(c)[1] >= ay - 4]
    return min(below, key=lambda c: abs(center(c)[1] - ay) + abs(center(c)[0] - ax) / 4, default=None)


def wait_widget(bridge: Bridge, find, timeout_s: float, poll_s: float = 1.5):
    """Poll full snapshots until `find(widgets)` returns something."""
    deadline = time.monotonic() + timeout_s
    while True:
        found = find(bridge.snap(every=True))
        if found:
            return found
        if time.monotonic() >= deadline:
            return None
        time.sleep(poll_s)
