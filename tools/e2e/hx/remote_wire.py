"""makepad-remote wire client over a window's own Unix socket.

The build's Makepad (.sources/makepad @ .jit/sources.lock, libs/remote) opens,
in every app unless MAKEPAD_REMOTE=0, a same-user endpoint:
  socket  /tmp/makepad-<uid>/makepad-remote-<pid>.sock  (dir 0700, socket 0600,
          peers checked with getpeereid; nothing on the network)
Framing: len u32 LE (excl. itself) | chan u16 LE | kind u8 | flags u8 | payload.
Channel 0 carries HELLO (opcode 1) / HELLO_OK (2) / GOODBYE (4). A client opens
odd channels with OPEN "service\\nargs"; the `cmd` service answers one request
line with JSON lines ending in {"end":...}."""
import json
import os
import secrets
import socket
import stat
import struct
from pathlib import Path

OPEN, OK, DATA, CREDIT, CLOSE, PING, PONG = 1, 2, 3, 4, 5, 6, 7
OP_HELLO, OP_HELLO_OK, OP_GOODBYE = 1, 2, 4
FLAG_MORE = 1
MAX_FRAME = 1 << 20
TIMEOUT_S = 20


class WireError(RuntimeError):
    pass


def socket_path(pid: int) -> Path:
    return Path(f"/tmp/makepad-{os.getuid()}") / f"makepad-remote-{pid}.sock"


def check_private(path: Path):
    """Refuse a socket (or its dir) that is not this user's own and private."""
    for p, kind in ((path.parent, stat.S_ISDIR), (path, stat.S_ISSOCK)):
        st = os.lstat(p)
        if not kind(st.st_mode) or st.st_uid != os.getuid() or st.st_mode & 0o077:
            raise WireError(f"{p} is not this user's private {'dir' if kind is stat.S_ISDIR else 'socket'}")


def encode(chan: int, kind: int, payload: bytes, flags: int = 0) -> bytes:
    return struct.pack("<IHBB", len(payload) + 4, chan, kind, flags) + payload


def read_frame(sock) -> tuple:
    head = _read_exact(sock, 8)
    length, chan, kind, flags = struct.unpack("<IHBB", head)
    if length < 4 or length > MAX_FRAME:
        raise WireError(f"bad frame length {length}")
    return chan, kind, flags, _read_exact(sock, length - 4)


def _read_exact(sock, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        got = sock.recv(n - len(buf))
        if not got:
            raise WireError("endpoint closed the connection")
        buf += got
    return buf


class Session:
    """One connection, one `cmd` channel; requests run one after another."""

    def __init__(self, path: Path, connect=None):
        check_private(path)
        self.sock = (connect or _connect)(path)
        self.sock.sendall(encode(0, DATA, bytes([OP_HELLO]) + (
            f"major=1\nminor=0\nrole=client\nname=hagency-e2e\nlink_id={secrets.token_hex(8)}\n").encode()))
        self._expect_hello_ok()
        self.chan = 1
        self.sock.sendall(encode(self.chan, OPEN, b"cmd\n"))
        chan, kind, _, payload = self._next(self.chan)
        if kind != OK:
            raise WireError(f"cmd service refused: {payload[2:].decode(errors='replace')}")

    def _expect_hello_ok(self):
        chan, kind, _, payload = self._next(0)
        if kind != DATA or not payload:
            raise WireError("no HELLO_OK")
        if payload[0] == OP_GOODBYE:
            raise WireError(f"endpoint refused: {payload[1:].decode(errors='replace')}")
        if payload[0] != OP_HELLO_OK:
            raise WireError(f"unexpected control opcode {payload[0]}")

    def _next(self, want_chan: int) -> tuple:
        """Next frame for `want_chan`; answers PINGs, ignores other channels."""
        while True:
            chan, kind, flags, payload = read_frame(self.sock)
            if kind == PING:
                self.sock.sendall(encode(chan, PONG, payload))
                continue
            if chan == want_chan:
                if kind == CLOSE and want_chan:
                    code = struct.unpack("<H", payload[:2])[0] if len(payload) >= 2 else -1
                    raise WireError(f"channel closed ({code}): {payload[2:].decode(errors='replace')}")
                return chan, kind, flags, payload

    def request(self, line: str) -> list:
        """Send one command line; return its JSON lines (without the end line).
        A reply line {"err": ...} raises."""
        self.sock.sendall(encode(self.chan, DATA, line.encode() + b"\n"))
        rows, pending = [], b""
        while True:
            _, kind, flags, payload = self._next(self.chan)
            if kind != DATA:
                continue
            self.sock.sendall(encode(self.chan, CREDIT, struct.pack("<I", len(payload))))
            pending += payload
            if flags & FLAG_MORE:
                continue
            for raw in pending.decode(errors="replace").splitlines():
                if not raw.strip():
                    continue
                row = json.loads(raw) if raw.lstrip().startswith("{") else {"text": raw}
                if "end" in row:
                    return rows
                if "measured" in row:
                    continue
                if "err" in row:
                    raise WireError(f"{line.split()[0]}: {row['err']}")
                rows.append(row)
            pending = b""

    def close(self):
        try:
            self.sock.sendall(encode(self.chan, CLOSE, struct.pack("<H", 0)))
        except OSError:  # already gone: closing is all that is left to do
            pass
        self.sock.close()


def _connect(path: Path):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(TIMEOUT_S)
    s.connect(str(path))
    return s
