#!/usr/bin/env python3
"""Headless sign-in UI checks; credentials stay in a private Palpo fixture.

matrix.org is queried only for public discovery. Password login and restart
checks use the isolated fixture, never a user's real account.
"""
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time
import urllib.parse
import urllib.request

from native_probe import NativeApp


class LoginApp(NativeApp):
    def start(self):
        with socket.socket() as probe:
            if probe.connect_ex(("127.0.0.1", self.port)) == 0:
                raise RuntimeError("Native bridge already occupied")
        self.root.mkdir(parents=True, exist_ok=True)
        self.output.mkdir(parents=True, mode=0o700)
        profile = self.root / "profile"
        profile.mkdir(mode=0o700, exist_ok=True)
        (profile / "window_geom_state.json").write_text(json.dumps({"inner_size": list(self.size), "position": [50, 50], "is_fullscreen": False}))
        self.log = (self.output / "native.log").open("w")
        os.chmod(self.output / "native.log", 0o600)
        binary = Path("target/debug/rinx")
        self.trace.append({"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()})
        env = dict(os.environ, HAGENCY_DESKTOP_DATA_DIR=str(profile), MAKEPAD_REMOTE=str(self.port), MAKEPAD_HIDE_WINDOWS="1", MAKEPAD_NO_FOCUS="1")
        env.pop("MAKEPAD_FOCUS", None)
        self.process = subprocess.Popen([str(binary)], stdout=self.log, stderr=subprocess.STDOUT, env=env)
        for _ in range(100):
            if self.process.poll() is not None:
                raise RuntimeError("Native sign-in app exited")
            try:
                status = self.request("/s")
                assert status["pid"] == self.process.pid
                if status["w"]:
                    return self
            except (OSError, ValueError):
                pass
            time.sleep(.2)
        raise RuntimeError("Native bridge did not start")

    def fill(self, name, value, secret=False):
        self.click_id(name)
        self.request("/k", c="A", cmd=1, wait=1)
        if secret:
            # Do not record passwords in the input trace or error text.
            url = self.base + "/t?" + urllib.parse.urlencode({"t": value, "wait": 1})
            try:
                with urllib.request.urlopen(url, timeout=10) as response:
                    response.read()
            except Exception:
                raise RuntimeError("Private credential input failed") from None
            self.trace.append({"native_input": "password", "redacted": True})
        else:
            self.request("/t", t=value, wait=1)


def main():
    live = Path("lab/wechat-ux/evidence/live")
    fixture = json.loads((live / "fixture.json").read_text())
    root = live / ("login-check-" + str(time.time_ns()))
    app = LoginApp(root, size=(406, 900))
    result = {"passed": False, "checks": [], "evidence": str(app.output), "mode": "headless"}
    try:
        app.start()
        app.wait_text("Sign in to Rinx")
        app.capture("login-initial")
        app.click_id("check_server_button")
        app.wait_text("matrix-client.matrix.org", timeout=45)
        app.wait_text("Password · Browser SSO")
        app.capture("matrix-org-methods")
        result["checks"].append("matrix_org_public_discovery_advertises_password_and_sso")
        app.fill("homeserver_input", "javascript://invalid")
        app.click_id("browser_login_button")
        app.wait_text("Check your homeserver")
        app.click_text("Okay")
        result["checks"].append("invalid_server_rejected_before_browser_or_credentials")
        app.fill("homeserver_input", fixture["url"])
        app.click_id("check_server_button")
        app.wait_text("Password")
        app.click_id("browser_login_button")
        app.wait_text("does not advertise Matrix SSO", timeout=45)
        app.capture("unsupported-sso")
        app.click_text("Okay")
        result["checks"].append("unsupported_sso_recoverable_without_opening_browser")
        user = fixture["users"]["alex"]
        app.fill("user_id_input", user["user_id"])
        app.fill("password_input", user["password"] + "-invalid", secret=True)
        app.click_id("login_button")
        app.wait_text("Sign-in failed", timeout=60)
        app.capture("password-rejected")
        app.click_text("Okay")
        result["checks"].append("wrong_password_error_returns_to_form")
        app.fill("password_input", user["password"], secret=True)
        app.click_id("login_button")
        app.wait_text("Emma Wilson", timeout=90)
        app.capture("password-success")
        result["checks"].append("native_password_signin_and_real_palpo_room_sync")
        sessions = list((root / "profile").rglob("session"))
        assert len(sessions) == 1
        assert sessions[0].stat().st_mode & 0o777 == 0o600
        session = json.loads(sessions[0].read_text())
        assert session["user_session"]["user_id"] == user["user_id"]
        device = session["user_session"]["device_id"]
        result["checks"].append("session_persisted_owner_only")
        app.stop()
        app = LoginApp(root, size=(375, 812))
        app.start()
        app.wait_text("Emma Wilson", timeout=90)
        app.capture("session-restored")
        restored = json.loads(sessions[0].read_text())
        assert restored["user_session"]["device_id"] == device
        result["checks"].append("restart_restores_same_account_and_device_without_password")
        log = (app.output / "native.log").read_text()
        assert not any(line.startswith("[E]") for line in log.splitlines()), "Native runtime errors after restore"
        result["binary_sha256"] = hashlib.sha256(Path("target/debug/rinx").read_bytes()).hexdigest()
        result["restore_evidence"] = str(app.output)
        result["passed"] = True
    except Exception as error:
        result["error"] = str(error)
        raise
    finally:
        app.stop()
        (root / "result.json").write_text(json.dumps(result, indent=2))
        (live / "native-login.json").write_text(json.dumps(result, indent=2))
        print(json.dumps(result))


if __name__ == "__main__":
    main()
