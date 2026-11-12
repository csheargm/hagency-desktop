#!/usr/bin/env python3
"""Exercise the real native client through Makepad's loopback input bridge.

Uses a separate HAGENCY_DESKTOP_DATA_DIR, saves real PNGs and input/assertion traces, and
checks sends against the recipient's Matrix history. Never grants a visual score.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from seed import checked


class NativeUserIntervention(RuntimeError):
    """Automation stopped permanently because the person took control."""


class NativeApp:
    def __init__(self, root, port=8199, size=(406, 776), auto_login=True):
        self.root = Path(root).resolve()
        self.port = port
        self.base = f"http://127.0.0.1:{port}"
        self.trace = []
        self.process = None
        self.output = self.root / "native-runs" / uuid.uuid4().hex
        self.size = size
        self.auto_login = auto_login
        self._activity_checked = False
        self._user_seq = None
        self._interrupted = None

    def _interrupt(self, reason):
        if self._interrupted is None:
            self._interrupted = reason
            self.trace.append({"automation_interrupted": reason, "at": time.time()})
        raise NativeUserIntervention(reason)

    def _check_user_seq(self, headers):
        if self._user_seq is None:
            return
        for key in ("X-Makepad-User-Seq-Start", "X-Makepad-User-Seq"):
            value = headers.get(key)
            if value is None or value != str(self._user_seq):
                self._interrupt("Native user activity changed or became unverifiable; leaving the instance running")

    def _request_json(self, route, params):
        url = self.base + route + ("?" + urllib.parse.urlencode(params) if params else "")
        try:
            with urllib.request.urlopen(url, timeout=10) as response:
                self._check_user_seq(response.headers)
                return json.load(response), response.headers
        except urllib.error.HTTPError as error:
            if error.code == 409:
                self._interrupt("Makepad refused automation after native user activity; leaving the instance running")
            self._check_user_seq(error.headers)
            raise

    def _ensure_activity_guard(self):
        if self._activity_checked:
            return
        try:
            activity, headers = self._request_json("/activity", {})
        except urllib.error.HTTPError as error:
            # Older SDKs do not implement /activity or sequence headers.
            # A newer server's error must never silently disable its guard.
            if error.code != 404 or any(error.headers.get(key) is not None for key in
                                       ("X-Makepad-User-Seq", "X-Makepad-User-Seq-Start")):
                raise
            self._activity_checked = True
            self.trace.append({"activity_guard": "legacy_sdk_unavailable", "at": time.time()})
            return
        sequence = activity.get("user_seq")
        if type(sequence) is not int or sequence < 0 or type(activity.get("user_active")) is not bool:
            raise RuntimeError("Invalid Makepad activity response; refusing unguarded automation")
        self._user_seq = sequence
        self._check_user_seq(headers)
        if activity["user_active"] or activity.get("held"):
            self._interrupt("Native user input is active; leaving the instance running")
        self._activity_checked = True
        self.trace.append({"activity_guard": "enabled", "user_seq": sequence, "at": time.time()})

    def request(self, route, **params):
        if self._interrupted is not None:
            raise NativeUserIntervention(self._interrupted)
        self._ensure_activity_guard()
        if self._user_seq is not None:
            params["if_user_seq"] = self._user_seq
        if route in {"/click", "/m", "/mouse", "/k", "/key", "/t", "/text"}:
            self.trace.append({"native_input": route, "parameters": params, "at": time.time()})
        data, _ = self._request_json(route, params)
        return data

    def start(self):
        # An existing bridge may be another developer's app. Never drive/stop it.
        with socket.socket() as probe:
            if probe.connect_ex(("127.0.0.1", self.port)) == 0:
                raise RuntimeError(f"Bridge port {self.port} is already occupied")
        self.root.mkdir(parents=True, exist_ok=True)
        self.output.mkdir(parents=True, mode=0o700)
        profile = self.root / "profile"
        profile.mkdir(exist_ok=True)
        (profile / "window_geom_state.json").write_text(json.dumps({"inner_size": list(self.size), "position": [50, 50], "is_fullscreen": False}))
        fixture = json.loads((self.root / "fixture.json").read_text())
        args = ["target/debug/rinx"]
        if self.auto_login and not (profile / "latest_user_id.txt").exists():
            user = fixture["users"]["alex"]
            args += [user["user_id"], user["password"], fixture["url"]]
        self.log = (self.output / "native.log").open("w")
        os.chmod(self.output / "native.log", 0o600)
        self.trace.append({"run": self.output.name, "binary_sha256": hashlib.sha256(Path(args[0]).read_bytes()).hexdigest(), "at": time.time()})
        self.process = subprocess.Popen(args, stdout=self.log, stderr=subprocess.STDOUT,
                                        env=dict(os.environ, HAGENCY_DESKTOP_DATA_DIR=str(profile), MAKEPAD_REMOTE=str(self.port), RUST_BACKTRACE="1"))
        (self.root / "native-pid").write_text(str(self.process.pid))
        for _ in range(80):
            if self.process.poll() is not None:
                raise RuntimeError("Rinx exited during startup")
            try:
                status = self.request("/s")
                if status["pid"] != self.process.pid:
                    raise RuntimeError("Bridge belongs to a different process")
                if status["w"]:
                    return self
            except (OSError, ValueError):
                pass
            time.sleep(.25)
        raise RuntimeError("Native bridge did not start")

    def snap(self):
        return [w for w in self.request("/snap")["s"] if w["r"][2] > 0 and w["r"][3] > 0]

    def click(self, x, y):
        self.trace.append({"input": "click", "x": x, "y": y, "at": time.time()})
        self.request("/click", x=x, y=y, wait=1)
        time.sleep(.45)

    def click_id(self, widget_id):
        widgets = [w for w in self.request("/snap", all=1)["s"] if w["i"] == widget_id and w["r"][2] > 0 and w["r"][3] > 0]
        if not widgets:
            raise AssertionError(f"Missing visible widget: {widget_id}")
        x, y, width, height = widgets[0]["r"]
        self.click(x + width / 2, y + height / 2)

    def ocr(self):
        capture = self.request("/g")
        binary = Path("target/robrix-ux-ocr")
        source = Path(__file__).with_name("ocr.swift")
        if not binary.exists() or binary.stat().st_mtime < source.stat().st_mtime:
            subprocess.run(["swiftc", str(source), "-o", str(binary)], check=True, capture_output=True)
        rows = json.loads(subprocess.check_output([str(binary), capture["png"]], text=True))
        self.trace.append({"inspection": "screenshot_ocr", "sha256": hashlib.sha256(Path(capture["png"]).read_bytes()).hexdigest(), "rows": rows, "at": time.time()})
        return rows

    def click_text(self, text):
        row = next((row for row in self.ocr() if text in row["text"]), None)
        if row is None:
            raise AssertionError(f"Text absent from rendered screenshot: {text}")
        width, height = self.request("/s")["w"][0]["sz"]
        x, y, w, h = row["box"]
        self.click((x+w/2)*width, (y+h/2)*height)

    def wait_text(self, text, timeout=20, pixels=False):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                found = text in " ".join(row["text"] for row in self.ocr()) if pixels else any(text in (w.get("t") or "") for w in self.snap())
            except urllib.error.HTTPError as error:
                # The bridge can report a window before its first draw has
                # produced a snapshot. Retry only that transient response.
                if error.code != 404:
                    raise
                time.sleep(.25)
                continue
            if found:
                self.trace.append({"assert": "visible_text", "text": text, "passed": True, "at": time.time()})
                return
            time.sleep(.25)
        self.trace.append({"assert": "visible_text", "text": text, "passed": False, "at": time.time()})
        raise AssertionError(f"Native text not visible: {text}")

    def capture(self, name):
        capture = self.request("/g")
        path = self.root / (name + ".png")
        shutil.copyfile(capture["png"], path)
        shutil.copyfile(path, self.output / path.name)
        self.trace.append({"capture": path.name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest(), "dimensions": capture["sz"], "at": time.time()})
        return path

    def stop(self):
        if not self.process:
            return
        if self.process.poll() is None and self._interrupted is None:
            try:
                self.request("/gq")
                self.process.wait(timeout=10)
            except NativeUserIntervention:
                pass
            except (OSError, subprocess.TimeoutExpired):
                if self._activity_checked and self._user_seq is None:
                    self.process.terminate()
                    try:
                        self.process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        self.process.kill()
                        self.process.wait(timeout=3)
                else:
                    # Guarded SDKs must also authorize cleanup. Never bypass a
                    # missing response or a human's intervention with a kill.
                    try:
                        self.request("/quit")
                        self.process.wait(timeout=10)
                    except NativeUserIntervention:
                        pass
                    except (OSError, subprocess.TimeoutExpired):
                        self.trace.append({"cleanup": "guarded_shutdown_unconfirmed", "at": time.time()})
        if self.process.poll() is None:
            print(f"Native instance left running: pid={self.process.pid} {self.base}; "
                  + (self._interrupted or "guarded shutdown could not be confirmed"), file=sys.stderr)
        self.log.close()
        (self.root / "native-input-trace.json").write_text(json.dumps(self.trace, indent=2))
        (self.output / "trace.json").write_text(json.dumps(self.trace, indent=2))
        shutil.copyfile(self.output / "native.log", self.root / "native.log")
        os.chmod(self.root / "native.log", 0o600)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default="lab/wechat-ux/evidence/live")
    parser.add_argument("--port", type=int, default=8199)
    parser.add_argument("--startup-checks", type=int, default=3)
    args = parser.parse_args()
    for index in range(args.startup_checks):
        startup = NativeApp(args.root, args.port)
        try:
            startup.start()
            startup.wait_text("Emma Wilson")
            startup.capture("startup-chats")
            (startup.output / "result.json").write_text(json.dumps({"passed": True, "check": "restored_session_first_draw", "index": index}))
        finally:
            startup.stop()
    app = NativeApp(args.root, args.port)
    try:
        app.start()
        app.wait_text("Emma Wilson")
        app.capture("chats-native")
        app.click_id("contacts_tab")
        app.wait_text("Emma Wilson")
        app.capture("contacts-native")
        app.click_text("Group Chats")
        app.wait_text("Design Studio")
        app.wait_text("Weekend Plans")
        app.capture("groups-native")
        app.click_id("left")
        app.wait_text("Emma Wilson")
        app.click(155, 298)
        app.wait_text("Messages")
        app.capture("contact-profile-native")
        app.click_id("left")
        app.click_id("me_tab")
        app.wait_text("Alex Chen")
        app.capture("me-native")
        app.click_text("Settings")
        app.wait_text("Account and Security")
        app.capture("settings-native")
        app.click_text("Privacy")
        app.wait_text("Privacy", pixels=True)
        app.capture("privacy-native")
        app.click_id("mobile_back")
        app.wait_text("Account and Security")
        app.click_id("mobile_back")
        app.wait_text("Alex Chen")
        app.click_id("discover_tab")
        app.wait_text("Explore Groups")
        app.capture("discover-native")
        app.click_id("chats_tab")
        app.wait_text("Emma Wilson")
        fixture = json.loads((app.root / "fixture.json").read_text())
        room = fixture["rooms"]["emma"]
        for option, expected_kind, should_notify in [
            ("notify_mute", "override", False),
            ("notify_mentions", "room", False),
            ("notify_all", "room", True),
            ("notify_default", None, False),
        ]:
            row = next(w for w in app.snap() if w.get("t") == "Emma Wilson")
            x, y = row["r"][0] + 30, row["r"][1] + 6
            app.trace.append({"input": "secondary_click", "x": x, "y": y, "at": time.time()})
            app.request("/click", x=x, y=y, b=1, wait=1)
            app.wait_text("Notifications")
            app.click_id("notifications_button")
            app.wait_text("Mute Notifications")
            app.click_id(option)
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                rules = checked(fixture["url"], "GET", "pushrules/", token=fixture["users"]["alex"]["access_token"])["global"]
                explicit = [(kind, rule) for kind in ("room", "override") for rule in rules.get(kind, []) if rule["rule_id"] == room]
                if expected_kind is None:
                    matched = not explicit
                else:
                    matched = len(explicit) == 1 and explicit[0][0] == expected_kind and explicit[0][1]["enabled"] and (("notify" in explicit[0][1]["actions"]) == should_notify)
                if matched:
                    break
                time.sleep(.25)
            else:
                raise AssertionError(f"Native notification choice did not persist on Palpo: {option}")
            app.trace.append({"assert": "palpo_room_push_rules", "option": option, "passed": True, "at": time.time()})
            # Let the transient success toast dismiss before the next gesture.
            time.sleep(3.2)
        row = next(w for w in app.snap() if w.get("t") == "Emma Wilson")
        app.click(row["r"][0] + 30, row["r"][1] + 6)
        app.wait_text("Message (unencrypted)", pixels=True)
        app.capture("conversation-native")
        # Input goes through the native TextInput, not a direct server send.
        app.click_text("Message (unencrypted)")
        body = "Native Rinx live check " + str(time.time_ns())
        app.request("/t", t=body, wait=1)
        app.trace.append({"input": "text", "text": body, "at": time.time()})
        app.click_text("Send")
        app.wait_text("Native Rinx live check", pixels=True)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            events = checked(fixture["url"], "GET", f"rooms/{room}/messages?dir=b&limit=30", token=fixture["users"]["emma"]["access_token"])["chunk"]
            if any(event.get("content", {}).get("body") == body for event in events):
                break
            time.sleep(.3)
        else:
            raise AssertionError("Native send did not reach the Matrix recipient")
        app.trace.append({"assert": "recipient_received_native_send", "passed": True, "room_id": room, "at": time.time()})
        app.capture("sent-native")
        log = (app.output / "native.log").read_text(errors="replace")
        script_errors = [line for line in log.splitlines() if line.startswith("[E]")]
        assert not script_errors, "Native script errors: " + "\n".join(script_errors[:5])
        result = {"passed": True, "captures": sum("capture" in entry for entry in app.trace), "recipient_confirmed": True, "evidence": str(app.output)}
        (app.output / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps(result))
    finally:
        app.stop()


if __name__ == "__main__":
    main()
