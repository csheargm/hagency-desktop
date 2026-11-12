#!/usr/bin/env python3
"""Exercise the shipped pool allowance widgets in an offline native host.

Build: cargo build --release --locked --features onboarding-test-support --example hagency_visual
No Matrix client, executor or server is started. All profiles and replies are fixtures.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import http.client
import socket
import subprocess
import time
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


class App:
    def __init__(self, output, binary, language, narrow, calendar=None):
        self.out = output
        self.profile = output / "profile"
        (self.profile / "sandbox").mkdir(parents=True)
        (self.profile / "ui-language.json").write_text(json.dumps(language))
        self.pool = dict(id="pool", label="codex-pool-01", ownerMxid="@owner:example.test",
                         revision=7, grantRevision=1, publishedTokens=1000000,
                         nativeAvailableTokens=None, canRequest=True, blockingReason=None,
                         capabilities=["architect", "coding", "documentation", "integration", "review", "testing"])
        if calendar:
            self.pool.update(calendar=calendar, periodResetLabel="2026-11-09 00:00 PST · America/Los_Angeles")
        self.state_path = self.profile / "sandbox/onboarding-v1.json"
        self.state_path.write_text(json.dumps(dict(version=1, account="@applicant:example.test", workspace="manager",
            wizard=dict(step=0, runtime="", model=None, reasoning=None, tokens="1000000", label="", coordinator="", resource=None, admission=None),
            command=None, poolDraft=dict(pool=self.pool, name="小蓝", description="秘书", capability="architect",
                                        tokens="8000000", roomCommand="room-original", roomId="!offline:example.test", review=False))))
        self.fixtures = {
            "hagency.session": dict(protocol="hagency-management-v1", account="@applicant:example.test",
                administrator="@owner:example.test", device="offline-device", deviceGeneration=1,
                canAdmitResources=False, features=["resource-pool-agents-v1"]),
            "hagency.intent.pending": {"commands": []},
            "hagency.pools.list": {"items": [self.pool], "nextAfter": None},
            "hagency.pools.agents.list": {"items": [], "nextAfter": None},
        }
        self.fixture_path = output / "fixtures.json"
        self.fixture_path.write_text(json.dumps(self.fixtures))
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        self.base = f"http://127.0.0.1:{port}"
        self.trace = []
        self.log = (output / "native.log").open("w")
        self.process = subprocess.Popen([str(binary), f"--remote={port}"] + (["--narrow"] if narrow else []),
            cwd=ROOT, stdout=self.log, stderr=subprocess.STDOUT,
            env=dict(os.environ, HAGENCY_DESKTOP_DATA_DIR=str(self.profile), HAGENCY_UX_FIXTURES=str(self.fixture_path), HAGENCY_UX_SOURCE=str(ROOT / "apps/hagency/bundle/main.splash"),
                     MAKEPAD_HIDE_WINDOWS="1", MAKEPAD_NO_FOCUS="1"))
        self.wait(lambda: self.req("/s")["pid"] == self.process.pid)
        self.wait(lambda: any("Continue saved" in r.get("t", "") or "继续已保存" in r.get("t", "") for r in self.snap()))
        row = next(r for r in self.snap() if "Continue saved" in r.get("t", "") or "继续已保存" in r.get("t", ""))
        self.click_row(row)
        self.wait(lambda: self.row("pool_tokens").get("t") == "8000000" and "1 M" in self.row("pool_limit").get("t", "") and not self.row("pool_review")["enabled"])

    def req(self, path, **query):
        self.trace.append({"path": path, "query": query})
        with urllib.request.urlopen(self.base + path + ("?" + urllib.parse.urlencode(query) if query else ""), timeout=10) as response:
            return json.load(response)

    def wait(self, predicate):
        for _ in range(100):
            if self.process.poll() is not None:
                raise AssertionError("Offline host exited")
            try:
                if predicate():
                    return
            except (OSError, KeyError, StopIteration):
                pass
            time.sleep(.1)
        raise AssertionError("Native condition not reached")

    def snap(self):
        return [r for r in self.req("/snap", all=1)["s"] if r.get("ty") != "Splash"]

    def row(self, name):
        return next(r for r in self.snap() if r.get("i") == name)

    def visible(self, name):
        width, height = self.req("/s")["w"][0]["sz"]
        for attempt in range(24):
            row = self.row(name)
            x, y, w, h = row["r"]
            if w > 0 and h > 0 and 200 <= y and y + h <= height - 20:
                assert 0 <= x and x + w <= width, (name, row)
                return row
            self.req("/m", k="scroll", x=width/2, y=height-100,
                     dy=-10000 if attempt == 0 and h == 0 else (-200 if y < 200 and h > 0 else 200), wait=1)
        raise AssertionError("Unreachable control " + name)

    def click_row(self, row):
        x, y, w, h = row["r"]
        self.req("/click", x=x+w/2, y=y+h/2, wait=1)
        time.sleep(.1)

    def click(self, name):
        self.click_row(self.visible(name))

    def amount(self):
        return json.loads(self.state_path.read_text())["poolDraft"]["tokens"]

    def enter(self, value, separate=False):
        self.visible("pool_tokens")
        self.click_row(self.row("input"))
        self.req("/k", c="A", cmd=1, wait=1)
        raw_value=value
        value=format(int(value)/1_000_000,".6f").rstrip("0").rstrip(".")
        if separate:
            self.req("/k", c="Backspace", wait=1)
            for char in value:
                self.req("/t", t=char, wait=1)
        else:
            self.req("/t", t=value, wait=1)
        self.wait(lambda: self.amount() == raw_value)

    def shot(self, name):
        (self.out / (name + ".json")).write_text(json.dumps(self.snap(), ensure_ascii=False, indent=2))
        (self.out / (name + ".png")).write_bytes(urllib.request.urlopen(self.base + "/g?raw=1", timeout=10).read())

    def run(self):
        assert not self.row("pool_review")["enabled"]
        assert "8 M" in self.row("pool_amount_error")["t"]
        self.visible("pool_review")
        self.shot("01-over-limit")
        self.click("pool_quarter")
        self.wait(lambda: self.amount() == "250000")
        self.wait(lambda: self.row("pool_review")["enabled"])
        # Real pointer movement exercises the adapter and its isolate callback.
        x, y, w, h = self.visible("pool_slider")["r"]
        self.req("/m", k="down", x=x+w*.25, y=y+h/2, wait=1)
        self.req("/m", k="move", x=x+w*.5, y=y+h/2, wait=1)
        self.req("/m", k="up", x=x+w*.5, y=y+h/2, wait=1)
        moved = int(self.amount())
        assert 400000 < moved < 650000, moved
        self.req("/k", c="ArrowRight", wait=1)
        self.wait(lambda: int(self.amount()) == moved + 10000)
        self.enter("123456", separate=True)
        assert self.row("pool_tokens")["t"] == "123456"
        self.req("/event", data="ux:dark")
        time.sleep(.3)
        assert self.amount() == "123456"
        self.visible("pool_review")
        self.shot("02-dark-valid")
        self.req("/event", data="ux:violet")
        time.sleep(.3)
        self.enter("1000001")
        assert not self.row("pool_review")["enabled"]
        self.click("pool_max")
        assert self.amount() == "1000000"
        self.click("pool_review")
        self.wait(lambda: self.row("pool_submit")["enabled"])
        # The server limit changes between review and submit: no mutation fixture
        # exists, so a successful check must return to editing before that call.
        self.pool["publishedTokens"] = 500000
        self.pool["grantRevision"] = 2
        self.fixture_path.write_text(json.dumps(self.fixtures))
        self.req("/event", data="ux:reload-fixtures")
        self.click("pool_submit")
        self.wait(lambda: "0.5 M" in self.row("pool_limit")["t"])
        assert self.amount() == "1000000" and not self.row("pool_review")["enabled"]
        assert self.row("pool_failure")["t"]
        state = json.loads(self.state_path.read_text())
        assert state["command"] is None and state["poolDraft"]["roomId"] == "!offline:example.test"
        self.visible("pool_review")
        self.shot("03-changed-limit")
        self.click("pool_half")
        assert self.amount() == "250000" and self.row("pool_review")["enabled"]
        self.visible("pool_review")
        self.shot("04-violet-valid")

    def stop(self):
        if self.process.poll() is None:
            try:
                self.req("/quit")
            except (OSError, http.client.HTTPException):
                # The harness may exit before it answers /quit; its exit is what matters.
                pass
            self.process.wait(timeout=10)
        self.log.close()
        (self.out / getattr(self, "trace_name", "input-trace.json")).write_text(json.dumps(self.trace, indent=2))
        log = getattr(self, "log_path", self.out / "native.log").read_text()
        assert not any(s in log for s in ["[E]", "cannot find", "Shader compilation failed", "panicked at"]), "Inspect native.log for script or rendering errors"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/examples/hagency_visual")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    results = []
    for language, narrow in [("zh-CN", False), ("en", True)]:
        app = App(args.output.resolve() / (language + ("-narrow" if narrow else "-wide")), args.binary.resolve(), language, narrow)
        try:
            app.run()
            results.append({"language": language, "narrow": narrow, "passed": True})
        except Exception:
            app.shot("failure")
            raise
        finally:
            app.stop()
    (args.output / "report.json").write_text(json.dumps({"passed": True, "checks": results,
        "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest()}, indent=2) + "\n")
    print(json.dumps(results))


if __name__ == "__main__":
    main()
