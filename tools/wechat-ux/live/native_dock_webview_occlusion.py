#!/usr/bin/env python3
"""Offline regression for native webview visibility behind the Agent dock tab."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import threading
import time

from native_probe import NativeApp


class Page(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        body = b"<!doctype html><title>Dock visibility fixture</title><h1>Retained reader</h1><input value='Unsent browser draft'>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path("target/fast/examples/miniapp_dock"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--expect-fixed", action="store_true")
    parser.add_argument("--overlay", action="store_true", help="Use the production App in wizard_invite_review")
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=True)
    profile = root / "profile"
    profile.mkdir(exist_ok=True)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Page)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    app = NativeApp(root, port, auto_login=False)
    app.output.mkdir(parents=True)
    app.log = (app.output / "native.log").open("w")
    app.process = subprocess.Popen([str(args.binary.resolve())], stdin=subprocess.DEVNULL,
        stdout=app.log, stderr=subprocess.STDOUT,
        env=dict(os.environ, MAKEPAD_REMOTE=str(port), MAKEPAD_HIDE_WINDOWS="1", MAKEPAD_NO_FOCUS="1",
                 HAGENCY_DESKTOP_DATA_DIR=str(profile), RAYON_NUM_THREADS="1",
                 HAGENCY_DOCK_URL=f"http://127.0.0.1:{server.server_port}/"))
    report = {"passed": False, "checks": [], "states": {}, "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest()}

    def inspect(command, file):
        path = profile / file
        path.unlink(missing_ok=True)
        app.request("/event", data=command, wait=1)
        for _ in range(50):
            try:
                return json.loads(path.read_text())
            except (FileNotFoundError, json.JSONDecodeError):
                time.sleep(.1)
        raise AssertionError(f"Missing inspection: {file}")

    def state(name, browsers, visible):
        for _ in range(30):
            native = inspect("webview:inspect-native" if args.overlay else "dock:inspect-native",
                             "overlay-native.json" if args.overlay else "dock-native.json")
            if native["browsers"] == browsers and sum(w["visible_webviews"] for w in native["windows"]) == visible:
                break
            time.sleep(.1)
        workspace = inspect("webview:inspect" if args.overlay else "dock:inspect",
                            "overlay-inspection.json" if args.overlay else "dock-inspection.json")
        report["states"][name] = {"native": native, "workspace": workspace}
        (root / f"{name}.json").write_text(json.dumps(report["states"][name], indent=2) + "\n")
        app.capture(name)
        assert native["browsers"] == browsers, native
        assert sum(w["visible_webviews"] for w in native["windows"]) == visible, native
        assert sum(w["attached_webviews"] for w in native["windows"]) <= browsers, native
        return workspace

    def close_last_workspace_tab():
        buttons = [w for w in app.snap() if w["i"] == "close" and w["ty"] == "NavigationBarButton"]
        assert buttons, app.snap()
        x, y, width, height = max(buttons, key=lambda w: w["r"][0])["r"]
        app.click(x + width / 2, y + height / 2)

    try:
        for _ in range(100):
            if app.process.poll() is not None:
                raise RuntimeError("Offline dock fixture exited")
            try:
                if app.request("/s")["pid"] == app.process.pid:
                    break
            except OSError:
                pass
            time.sleep(.2)
        if args.overlay:
            app.request("/event", data="webview:open", wait=1)
            reader = state("reader", 1, 1)
            for cycle in range(3):
                app.request("/event", data="miniapp:open", wait=1)
                opened = state(f"miniapp-{cycle}", 1, 0)
                assert opened["miniapp_open"] and opened["reader_open"]
                app.request("/k", c="Escape", wait=1)
                retained = state(f"miniapp-escape-{cycle}", 1, 0)
                assert retained["reader_open"] and retained["browser"] == reader["browser"]
                app.request("/event", data="miniapp:back", wait=1)
                restored = state(f"reader-restored-{cycle}", 1, 1)
                assert not restored["miniapp_open"] and restored["reader_open"]
                assert restored["browser"] == reader["browser"] and restored["reader"] == reader["reader"]
            app.request("/event", data="management:open", wait=1)
            state("management", 1, 0)
            app.request("/event", data="miniapp:open", wait=1)
            state("nested-miniapp", 1, 0)
            app.request("/event", data="miniapp:back", wait=1)
            remaining = state("remaining-management", 1, 0)
            assert remaining["management_open"] and not remaining["miniapp_open"]
            app.request("/event", data="miniapp:back", wait=1)
            restored = state("all-panels-closed", 1, 1)
            assert not restored["management_open"] and not restored["miniapp_open"]
            assert restored["browser"] == reader["browser"] and restored["reader"] == reader["reader"]
            app.request("/event", data="webview:close", wait=1)
            state("reader-closed", 0, 0)
            report["checks"].append("Production App mini-app/management actions detach and restore one native browser; nested covers stay hidden until all close; final close releases native views")
            report["passed"] = True
            return
        app.wait_text("Conversation")
        initial = state("initial", 0, 0)
        app.click_id("web")
        reader = state("reader", 1, 1)["docked"]
        for cycle in range(3):
            app.click_id("agent_button")
            agent = state(f"agent-{cycle}", 1, 0)
            assert agent["docked"]["reader"] == reader["reader"]
            assert agent["docked"]["browser"] == reader["browser"]
            close_last_workspace_tab()
            restored = state(f"reader-restored-{cycle}", 1, 1)["docked"]
            assert restored["reader"] == reader["reader"]
            assert restored["browser"] == reader["browser"]
        report["checks"].append("Agent tab detaches the native reader; closing it restores the same browser and session, repeated three times")
        for theme in ["dark", "light"]:
            app.request("/event", data=f"dock:{theme}", wait=1)
            app.click_id("agent_button")
            themed = state(f"agent-{theme}", 1, 0)
            palette = themed["agent_palette"]
            if args.expect_fixed:
                assert palette["icon"] == palette["nav_fg"], palette
                assert palette["label"] == palette["nav_fg"], palette
            close_last_workspace_tab()
            state(f"reader-restored-{theme}", 1, 1)
        if args.expect_fixed:
            report["checks"].append("Agent icon and label use rail foreground after click and theme reapply")
        close_last_workspace_tab()
        state("reader-closed", 0, 0)
        report["checks"].append("Closing reader releases its native browser and attached views")
        report["passed"] = True
    except Exception as error:
        report["error"] = repr(error)
        raise
    finally:
        app.stop()
        server.shutdown()
        (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps({"passed":report["passed"],"checks":report["checks"],"error":report.get("error"),"output":str(root)}, indent=2))


if __name__ == "__main__":
    main()
