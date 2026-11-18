"""The makepad-remote wire client against an in-process fake endpoint."""
import json
import os
import socket
import struct
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from hx import bridge, remote_wire as rw  # noqa: E402


def fake_endpoint(sock, answers, seen):
    """HELLO_OK, OK to OPEN, then one JSON-lines answer per request."""
    _, _, _, hello = rw.read_frame(sock)
    seen.append(("hello", hello))
    sock.sendall(rw.encode(0, rw.PING, b"\0" * 8))
    sock.sendall(rw.encode(0, rw.DATA, bytes([rw.OP_HELLO_OK]) + b"major=1\nminor=0\nrole=endpoint\n"))
    chan, kind, _, payload = rw.read_frame(sock)
    while kind == rw.PONG:
        chan, kind, _, payload = rw.read_frame(sock)
    seen.append(("open", chan, kind, payload))
    sock.sendall(rw.encode(chan, rw.OK, b"cmd/1"))
    for answer in answers:
        while True:
            c, k, _, req = rw.read_frame(sock)
            if k == rw.DATA:
                break
        seen.append(("req", req))
        body = "\n".join(json.dumps(x) for x in answer).encode() + b"\n"
        half = len(body) // 2
        sock.sendall(rw.encode(chan, rw.DATA, body[:half], rw.FLAG_MORE))
        sock.sendall(rw.encode(chan, rw.DATA, body[half:]))


def private_socket_path(tmp):
    d = Path(tmp) / "sockdir"
    d.mkdir(mode=0o700)
    p = d / "s.sock"
    srv = socket.socket(socket.AF_UNIX)
    srv.bind(str(p))
    os.chmod(p, 0o600)
    return p, srv


class Wire(unittest.TestCase):
    def test_frame_layout(self):
        f = rw.encode(3, rw.DATA, b"hi", rw.FLAG_MORE)
        self.assertEqual(f[:8], struct.pack("<IHBB", 6, 3, rw.DATA, rw.FLAG_MORE))

    def test_handshake_open_and_reassembled_answer(self):
        a, b = socket.socketpair()
        seen = []
        answer = [{"measured": "x"}, {"app": "rinx", "pid": 7}, {"end": "status", "ms": 1}]
        t = threading.Thread(target=fake_endpoint, args=(b, [answer], seen), daemon=True)
        t.start()
        with tempfile.TemporaryDirectory(dir="/tmp") as tmp:
            path, srv = private_socket_path(tmp)
            s = rw.Session(path, connect=lambda _p: a)
            rows = s.request(json.dumps({"cmd": "status"}))
            srv.close()
        self.assertEqual(rows, [{"app": "rinx", "pid": 7}])
        self.assertEqual(seen[0][1][0], rw.OP_HELLO)
        self.assertEqual(seen[1][1:], (1, rw.OPEN, b"cmd\n"))

    def test_err_line_raises(self):
        a, b = socket.socketpair()
        t = threading.Thread(target=fake_endpoint, args=(b, [[{"err": "unknown command"}]], []), daemon=True)
        t.start()
        with tempfile.TemporaryDirectory(dir="/tmp") as tmp:
            path, srv = private_socket_path(tmp)
            s = rw.Session(path, connect=lambda _p: a)
            with self.assertRaisesRegex(rw.WireError, "unknown command"):
                s.request("nope")
            srv.close()

    def test_refuses_a_socket_others_can_reach(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as tmp:
            path, srv = private_socket_path(tmp)
            os.chmod(path, 0o666)
            with self.assertRaisesRegex(rw.WireError, "private socket"):
                rw.check_private(path)
            os.chmod(path, 0o600)
            os.chmod(path.parent, 0o755)
            with self.assertRaisesRegex(rw.WireError, "private dir"):
                rw.check_private(path)
            srv.close()

    def test_socket_path_is_per_pid_in_the_user_dir(self):
        self.assertEqual(str(rw.socket_path(42)), f"/tmp/makepad-{os.getuid()}/makepad-remote-42.sock")


class BridgeGuards(unittest.TestCase):
    def test_no_socket_means_bridge_not_up(self):
        with mock.patch.object(bridge, "window_pid", return_value=999999):
            with self.assertRaisesRegex(bridge.BridgeDown, "bridge not up yet"):
                bridge.Bridge("betty")

    def test_destructive_words_refused(self):
        b = bridge.Bridge.__new__(bridge.Bridge)
        b.owner, b.session = "betty", mock.Mock()
        for words in ("quit", "close", "gq", "input replay", "window"):
            with self.assertRaises(ValueError):
                b.cmd(words)
        b.session.request.assert_not_called()

    def test_acts_carry_the_persons_input_counter(self):
        b = bridge.Bridge.__new__(bridge.Bridge)
        b.owner, b.session = "betty", mock.Mock()
        b.session.request.side_effect = [[{"activity": {"user_active": False, "user_seq": 5}}], []]
        with mock.patch("time.sleep"):
            b.press({"id": "d45", "actions": "focus,press"})
        sent = json.loads(b.session.request.call_args_list[1].args[0])
        self.assertEqual((sent["cmd"], sent["node"], sent["if_user_seq"]), ("ui act press", "d45", 5))

    def test_person_using_the_window_stops_the_act(self):
        b = bridge.Bridge.__new__(bridge.Bridge)
        b.owner, b.session = "betty", mock.Mock()
        b.session.request.return_value = [{"activity": {"user_active": True, "user_seq": 9}}]
        with self.assertRaises(bridge.StepNotFound):
            b.press({"id": "d45", "actions": "press"})
        self.assertEqual(b.session.request.call_count, 1)

    def test_wire_failure_becomes_bridge_down(self):
        b = bridge.Bridge.__new__(bridge.Bridge)
        b.owner, b.session = "betty", mock.Mock()
        b.session.request.side_effect = rw.WireError("endpoint closed the connection")
        with self.assertRaises(bridge.BridgeDown):
            b.nodes("x")


if __name__ == "__main__":
    unittest.main()
