"""Minimal Matrix client-server API client (stdlib only).

Sign-in uses the same cookieless Pasion native API the desktop uses
(`/auth/api/v1/native/{login,register}` + PKCE), so personas are real accounts
created through the server's normal registration path."""
import base64
import hashlib
import json
import secrets
import time
import urllib.error
import urllib.parse
import urllib.request

LOOPBACK = "http://127.0.0.1:47999/"  # never listened on: only the code is read
SCOPE_API = "urn:matrix:org.matrix.msc2967.client:api:*"
SCOPE_DEVICE = "urn:matrix:org.matrix.msc2967.client:device:"
HTTP_TIMEOUT = 40
MAX_PAGES = 50
ALLOWED_ORIGINS = ("http://127.0.0.1:8090", "http://127.0.0.1:8092")


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """A redirect would carry the bearer token to wherever it points."""

    def redirect_request(self, *args, **kwargs):
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


class MatrixError(RuntimeError):
    """An HTTP or protocol failure, carrying status and server error code."""

    def __init__(self, where: str, status: int, body: str):
        super().__init__(f"{where}: HTTP {status}: {body[:300]}")
        self.status, self.body = status, body


def http(method: str, url: str, body=None, token=None, form=False, timeout=HTTP_TIMEOUT):
    if not url.startswith(tuple(o + "/" for o in ALLOWED_ORIGINS)):
        raise MatrixError(url.split("?")[0], 0, "only the alpha/beta demo servers may be called")
    headers = {"Accept": "application/json"}
    data = None
    if body is not None:
        if form:
            data = urllib.parse.urlencode(body).encode()
            headers["Content-Type"] = "application/x-www-form-urlencoded"
        else:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with _OPENER.open(req, timeout=timeout) as resp:
            text = resp.read().decode()
    except urllib.error.HTTPError as e:
        raise MatrixError(f"{method} {url.split('?')[0]}", e.code, e.read().decode(errors="replace")) from e
    return json.loads(text) if text else {}


def native_sign_in(origin: str, username: str, password: str, register: bool) -> dict:
    """Return {access_token, refresh_token, device_id} for a password account."""
    cfg = http("GET", f"{origin}/auth/api/v1/native/config")
    verifier = secrets.token_urlsafe(48)
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    device = "E2E" + secrets.token_hex(5).upper()
    body = {"username": username, "password": password, "client_id": cfg["client_id"],
            "redirect_uri": LOOPBACK, "scope": f"{SCOPE_API} {SCOPE_DEVICE}{device}",
            "state": secrets.token_urlsafe(12), "code_challenge": challenge,
            "code_challenge_method": "S256"}
    reply = http("POST", f"{origin}/auth/api/v1/native/{'register' if register else 'login'}", body)
    if reply.get("status") != "success" or not str(reply.get("redirect_url", "")).startswith(LOOPBACK):
        raise MatrixError("native sign-in", 200, str(reply.get("error", "unexpected reply")))
    query = urllib.parse.parse_qs(urllib.parse.urlparse(reply["redirect_url"]).query)
    meta = http("GET", f"{origin}/_matrix/client/v1/auth_metadata")
    token = http("POST", meta["token_endpoint"], {
        "grant_type": "authorization_code", "code": query["code"][0], "redirect_uri": LOOPBACK,
        "client_id": cfg["client_id"], "code_verifier": verifier}, form=True)
    return {"access_token": token["access_token"], "refresh_token": token.get("refresh_token"),
            "device_id": device, "client_id": cfg["client_id"], "token_endpoint": meta["token_endpoint"]}


def username_available(origin: str, username: str) -> bool:
    q = urllib.parse.urlencode({"username": username})
    return http("GET", f"{origin}/auth/api/v1/native/username-available?{q}").get("status") == "available"


class Client:
    """One signed-in account on one homeserver."""

    def __init__(self, origin: str, token: str, user_id: str = ""):
        self.origin, self.token = origin, token
        self.user_id = user_id or self.call("GET", "/account/whoami")["user_id"]
        self._txn = 0

    def call(self, method, path, body=None, timeout=HTTP_TIMEOUT):
        return http(method, f"{self.origin}/_matrix/client/v3{path}", body, self.token, timeout=timeout)

    def _txn_id(self) -> str:
        self._txn += 1
        return f"e2e{int(time.time() * 1000)}{self._txn}"

    def send(self, room: str, content: dict, etype: str = "m.room.message") -> str:
        path = f"/rooms/{q(room)}/send/{etype}/{self._txn_id()}"
        return self.call("PUT", path, content)["event_id"]

    def create_room(self, name: str, invite: list, preset="private_chat") -> str:
        body = {"name": name, "preset": preset, "invite": invite,
                "power_level_content_override": {"invite": 0}}
        return self.call("POST", "/createRoom", body)["room_id"]

    def invite(self, room: str, user: str):
        return self.call("POST", f"/rooms/{q(room)}/invite", {"user_id": user})

    def join(self, room: str, via=()):
        suffix = ("?" + urllib.parse.urlencode([("server_name", s) for s in via])) if via else ""
        return self.call("POST", f"/join/{q(room)}{suffix}", {})

    def leave(self, room: str):
        return self.call("POST", f"/rooms/{q(room)}/leave", {})

    def joined_rooms(self) -> list:
        return self.call("GET", "/joined_rooms")["joined_rooms"]

    def members(self, room: str) -> dict:
        chunk = self.call("GET", f"/rooms/{q(room)}/members")["chunk"]
        return {e["state_key"]: e["content"].get("membership") for e in chunk}

    def state(self, room: str, etype: str, key: str = ""):
        return self.call("GET", f"/rooms/{q(room)}/state/{etype}/{q(key)}")

    def messages_after(self, room: str, since_ms: int, limit: int = 100, max_pages: int = MAX_PAGES) -> list:
        """Timeline events (oldest first) with origin_server_ts >= since_ms.
        Follows `end` tokens: a server may answer with a short page (Palpo
        returned 4 events for limit=300), so one page is not "everything"."""
        flt = urllib.parse.quote(json.dumps({"lazy_load_members": True}))
        events, token = [], ""
        for _ in range(max_pages):
            frm = f"&from={urllib.parse.quote(token)}" if token else ""
            res = self.call("GET", f"/rooms/{q(room)}/messages?dir=b&limit={limit}&filter={flt}{frm}")
            if "chunk" not in res:
                raise MatrixError("messages", 0, "reply without a chunk")
            chunk = res["chunk"]
            events += [e for e in chunk if e.get("origin_server_ts", 0) >= since_ms]
            if not chunk or not res.get("end") or min(e.get("origin_server_ts", 0) for e in chunk) < since_ms:
                return sorted(events, key=lambda e: e["origin_server_ts"])
            token = res["end"]
        raise MatrixError("messages", 0, f"more than {max_pages} pages back to {since_ms}; window too wide")

    def event(self, room: str, event_id: str) -> dict:
        return self.call("GET", f"/rooms/{q(room)}/event/{q(event_id)}")

    def relations(self, room: str, event_id: str) -> list:
        path = f"/rooms/{q(room)}/relations/{q(event_id)}?limit=100"
        return http("GET", f"{self.origin}/_matrix/client/v1{path}", None, self.token).get("chunk", [])

    def display_name(self, user: str) -> str:
        try:
            return self.call("GET", f"/profile/{q(user)}/displayname").get("displayname", "")
        except MatrixError as e:  # profile not visible yet: callers fall back to the MXID
            if e.status == 404:
                return ""
            raise


def q(value: str) -> str:
    return urllib.parse.quote(value, safe="")
