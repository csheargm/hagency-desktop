#!/usr/bin/env python3
"""Local-agent settings in the offline Hagency host: Agent menu list, the
local agent page and its model/limits editors. Headless; fixtures only.

No Matrix client, executor or server is started and no request is sent. Saves
one PNG per screen for review. Build first:
cargo build --release --locked --features onboarding-test-support --example hagency_visual
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
AGENT = "agt_01xiaobaicai"

def pool_request(**changes):
    request = dict(
        id="par_xiaobai", definition=dict(name="小白", description="程序员", capability="architect"),
        state="approved_waiting_executor", revision=2, requestedTokens=1000000, poolId="pool",
        poolLabel="codex-pool-01", resourceOwnerMxid="@dspfac:example.test", managerMxid="@edward:example.test",
        membershipState="matrix_verified", assignmentId="asg_1", reservationState="reserved",
        reservationReason=None, runtimeState=None, canCancel=True, canDecide=False,
        canStopSupply=False, blockingReason=None)
    request.update(changes)
    return request

def fixtures(encrypted=True, running=True, here=True, online=True, status_error=None, empty=False, hold_service=False,
             save_error=None, pool=None, account="@edward:example.test", pools=None):
    observation = dict(policyRevision=3, budget=dict(kind="tokens", tokens=2000000, period="utc_month"),
                       requests="allow", highRiskTools="deny", toolRuleCount=0, directoryRuleCount=0,
                       knownConsumedTokens=885626, heldTokens=0,
                       model=dict(model="gpt-6.1-terra", reasoningEffort="high", webSearch=True, workspaceConfigured=True))
    status = dict(agentId=AGENT, here=here, deviceName="This Mac" if here else "Edward 的 MacBook Pro",
                  service=dict(state=("running" if running else "stopped") if here else "elsewhere"),
                  chat=dict(roomId="!dm:example.test", encrypted=encrypted),
                  rooms=[dict(id="bnd_project", name="Hagency-Project", state="active")], resetLocalTime="08:00")
    return {
        "hagency.session": dict(protocol="hagency-management-v1", account=account,
            administrator="@admin:example.test", device="offline-device", deviceGeneration=1,
            canAdmitResources=False, features=["resource-pool-agents-v1"]),
        "hagency.intent.pending": {"commands": []},
        "hagency.pools.list": {"items": pools or [], "nextAfter": None},
        "hagency.pools.agents.list": {"items": [pool_request(**(pool or {}))], "nextAfter": None},
        "hagency.pools.agents.get": {"request": pool_request(**(pool or {}))},
        "hagency.personal_agents.list": {"protocol": "hagency-personal-settings-v1",
            "items": [] if empty else [dict(id=AGENT, displayName="小白菜", state="active")], "nextAfter": None},
        "hagency.personal_agents.get": {"protocol": "hagency-personal-settings-v1",
            "agent": dict(id=AGENT, displayName="小白菜", executionDeviceId="dev_here", generation=1, state="active"),
            "settings": dict(supported=True, online=online, cached=not online, observedAtMs=1793334588000,
                             observedAtUtc="2026-10-29 04:29:48 UTC", observedAtLocal="2026-10-29 12:29",
                             observation=observation)},
        "hagency.personal_agents.status": {"_cases": [{"when": {"id": AGENT}, "error": status_error}]} if status_error else status,
        "hagency.personal_agents.service": {"_hold": True} if hold_service else status,
        "hagency.intent.new": {"commandId": "cmd_offline_1"},
        "hagency.intent.acknowledge": {},
        "hagency.personal_agents.policy.save": {"_cases": [{"when": {}, "error": save_error or "offline_visual_fixture_unavailable"}]},
        "hagency.personal_agents.model.choices": dict(agentId=AGENT, generation=1, profileDigest="digest",
            workspaceConfigured=True, canChooseWorkspace=True,
            current=dict(runtime="codex", model="gpt-6.1-terra", reasoningEffort="high", webSearch=True),
            models=[dict(runtime="codex", model="gpt-6.1-terra", displayName="GPT-6.1 Terra",
                         reasoningEfforts=["low", "medium", "high", "max"]),
                    dict(runtime="codex", model="gpt-6.1-mini", displayName="GPT-6.1 Mini",
                         reasoningEfforts=["low", "medium", "high"])]),
    }

class Host:
    def __init__(self, binary, output, language, narrow, workspace="manager", fixture_overrides=None, source=None, **variant):
        self.out = output
        profile = output / "profile"
        (profile / "sandbox").mkdir(parents=True, exist_ok=True)
        (profile / "ui-language.json").write_text(json.dumps(language))
        (profile / "sandbox/onboarding-v1.json").write_text(json.dumps(dict(version=1,
            account=variant.get("account", "@edward:example.test"), workspace=workspace, command=None,
            wizard=dict(step=0, runtime="", model=None, reasoning=None, tokens="1000000", label="",
                        coordinator="", resource=None, admission=None))))
        self.fixture_path = output / "fixtures.json"
        self.fixture_path.write_text(json.dumps(dict(fixtures(**variant), **(fixture_overrides or {}))))
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        self.base = f"http://127.0.0.1:{port}"
        self.log = (output / "native.log").open("w")
        self.process = subprocess.Popen([str(binary), f"--remote={port}"] + (["--narrow"] if narrow else []),
            cwd=ROOT, stdout=self.log, stderr=subprocess.STDOUT,
            env=dict(os.environ, HAGENCY_DESKTOP_DATA_DIR=str(profile), HAGENCY_UX_FIXTURES=str(self.fixture_path),
                     HAGENCY_UX_SOURCE=str(source or ROOT / "apps/hagency/bundle/main.splash"),
                     MAKEPAD_HIDE_WINDOWS="1", MAKEPAD_NO_FOCUS="1"))
        def ready():
            status = self.req("/s")
            return status["pid"] == self.process.pid and bool(status.get("w"))
        self.wait(ready, "host start")

    def req(self, path, **query):
        url = self.base + path + ("?" + urllib.parse.urlencode(query) if query else "")
        with urllib.request.urlopen(url, timeout=15) as response:
            return json.load(response)

    def wait(self, predicate, what, tries=120):
        for _ in range(tries):
            if self.process.poll() is not None:
                raise AssertionError("Offline host exited; see native.log")
            try:
                if predicate():
                    return
            except (OSError, KeyError, StopIteration, IndexError):
                pass
            time.sleep(.15)
        raise AssertionError("Not reached: " + what)

    def snap(self):
        # Splash's root entry carries the whole program text; it is not UI.
        return [w for w in self.req("/snap")["s"] if w.get("i") != "card"]

    def row(self, name):
        return next(w for w in self.snap() if w.get("i") == name and w["r"][2] > 0)

    def reveal(self, name):
        # /snap lists only drawn widgets: scroll down until the control is
        # fully inside the window, back up if it is above the content area.
        height = self.req("/s")["w"][0]["sz"][1]
        for _ in range(40):
            snap = self.snap()
            # The scroll area starts under the header; its top moves with the header's height.
            top = next((w["r"][1] + 4 for w in snap if w.get("i") == "content" and w["r"][3] > 0), 210)
            footer = [w["r"][1] for w in snap if w.get("i") == "primary" and w["r"][2] > 0]
            bottom = min([height - 20] + [y - 24 for y in footer])
            rows = [w for w in snap if w.get("i") == name and w["r"][2] > 0]
            if rows:
                x, y, w, h = rows[0]["r"]
                if y >= top and y + h <= bottom:
                    return
                self.scroll(200 if y + h > bottom else -200)
            else:
                self.scroll(200)
        raise AssertionError("Not reachable on screen: " + name)

    def click(self, name, content=True):
        if content:
            self.reveal(name)
        else:
            self.wait(lambda: self.row(name), name)
        x, y, w, h = self.row(name)["r"]
        self.req("/click", x=x + w / 2, y=y + h / 2, wait=1)
        time.sleep(.6)

    def click_text(self, text):
        self.wait(lambda: next(w for w in self.snap() if w.get("t") == text and w["r"][2] > 0), text)
        x, y, w, h = next(w for w in self.snap() if w.get("t") == text and w["r"][2] > 0)["r"]
        self.req("/click", x=x + w / 2, y=y + h / 2, wait=1)
        time.sleep(.6)

    def scroll(self, dy):
        width, height = self.req("/s")["w"][0]["sz"]
        self.req("/m", k="scroll", x=width / 2, y=height * .6, dy=dy, wait=1)
        time.sleep(.4)

    def capture(self, name):
        png = self.req("/g", scale=1)["png"]
        target = self.out / f"{name}.png"
        shutil.copyfile(png, target)
        print("captured", target, flush=True)

    def event(self, data):
        # The remote answers /event only once the app has handled it, within 4 s.
        # A busy app (a theme reapply on a loaded machine) misses that window,
        # but the event stays queued and runs before any later command, so a
        # capture after it still sees its effect. Any other error still fails.
        try:
            self.req("/event", data=data)
        except urllib.error.HTTPError as error:
            if error.code != 404 or "timeout" not in error.read().decode(errors="replace"):
                raise
        time.sleep(1.0)

    def close(self):
        try:
            self.req("/quit")
        except OSError:
            pass
        try:
            self.process.wait(10)
        except subprocess.TimeoutExpired:
            self.process.kill()

def open_agent(host):
    host.click("agents", content=False)
    host.wait(lambda: host.row("local_agent_open"), "local agent list")
    host.click("local_agent_open")
    host.wait(lambda: host.row("personal_back"), "local agent page")
    time.sleep(.8)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/examples/hagency_visual")
    parser.add_argument("--language", default="zh-CN")
    parser.add_argument("--wide", action="store_true")
    parser.add_argument("--only", default="", help="comma-separated run names, such as project,project-pending")
    a = parser.parse_args()
    a.output.mkdir(parents=True, exist_ok=True)
    narrow = not a.wide
    def run(name, steps, **variant):
        if a.only and name not in a.only.split(","):
            return
        host = Host(a.binary, a.output / name, a.language, narrow, **variant)
        try:
            steps(host)
        finally:
            host.close()

    def main_flow(host):
        host.click("agents", content=False)
        host.wait(lambda: host.row("local_agent_open"), "local agent list")
        host.capture("01-agent-tab")
        host.click("local_agent_open")
        host.wait(lambda: host.row("personal_back"), "local agent page")
        time.sleep(.8)
        host.capture("02-agent-top")
        host.scroll(420)
        host.capture("03-agent-middle")
        host.scroll(900)
        host.capture("04-agent-bottom")
        host.click("local_retire")
        host.wait(lambda: host.row("local_retire_confirm"), "retire alert")
        host.capture("04b-retire-alert")
        host.click("local_retire_cancel")
        host.scroll(-100000)
        host.click("personal_edit")
        host.wait(lambda: host.row("personal_edit_cancel", ), "limits editor")
        host.capture("05-limits-edit")
        host.click_text("先问我")
        host.capture("05b-limits-changed")
        host.click("overview", content=False)
        host.wait(lambda: host.row("local_discard_confirm"), "leave alert")
        host.capture("05c-leave-with-changes")
        host.click("local_discard_keep")
        host.click("personal_review", content=False)
        host.wait(lambda: host.row("personal_submit"), "review")
        host.capture("06-limits-review")
        host.click("personal_review_back", content=False)
        host.click("personal_edit_cancel", content=False)
        host.wait(lambda: host.row("local_discard_confirm"), "discard alert")
        host.capture("06b-discard-alert")
        host.click("local_discard_confirm")
        host.wait(lambda: host.row("personal_back"), "back to page")
        host.click("personal_model_edit")
        host.wait(lambda: host.row("personal_model_submit"), "model sheet")
        time.sleep(.6)
        host.capture("07-model")
        host.click_text("最大")
        host.click_text("GPT-6.1 Mini")
        host.capture("07d-model-mini")
        host.click_text("GPT-6.1 Terra")
        host.click_text("高")
        host.click("personal_model_search")
        host.reveal("personal_workspace_default")
        host.capture("07b-model-changed")
        host.click("personal_model_back", content=False)
        host.wait(lambda: host.row("local_discard_confirm"), "model discard alert")
        host.capture("07c-model-discard")
        host.click("local_discard_confirm")
        host.wait(lambda: host.row("personal_back"), "back from model")
        time.sleep(.6)
        host.click("personal_back", content=False)
        host.wait(lambda: host.row("local_agent_open"), "back to the list")
        time.sleep(.6)
        host.capture("07e-back-to-list")
        host.click("local_agent_open")
        host.wait(lambda: host.row("personal_back"), "local agent page again")
        time.sleep(.8)
        host.event("ux:dark")
        host.capture("08-agent-dark")
        host.scroll(420)
        host.capture("08b-agent-dark-middle")
        host.click("local_retire")
        host.wait(lambda: host.row("local_retire_confirm"), "dark retire alert")
        host.capture("08c-dark-alert")
        host.click("local_retire_cancel")
        host.scroll(-100000)
        host.event("ux:light")
        host.event("ux:large-text")
        host.capture("09-agent-large-text")
        host.scroll(420)
        host.capture("09b-large-text-middle")

    def encrypt_flow(host):
        open_agent(host)
        host.capture("10-plain-stopped")
        host.click("local_encrypt")
        host.wait(lambda: host.row("local_encrypt_confirm"), "encryption alert")
        host.capture("11-encrypt-alert")

    def capture_page(name):
        def steps(host):
            open_agent(host)
            host.capture(name)
        return steps

    def empty_flow(host):
        host.click("agents", content=False)
        host.wait(lambda: host.row("local_add"), "empty local agents")
        host.capture("15-empty-list")

    run("encrypted", main_flow)
    run("plain", encrypt_flow, encrypted=False, running=False)
    def offline_flow(host):
        open_agent(host)
        host.capture("12-device-offline")
        host.scroll(900)
        host.capture("12b-device-offline-bottom")
        host.event("ux:dark")
        host.capture("12c-device-offline-dark")

    def pending_flow(host):
        open_agent(host)
        host.click("local_service")
        time.sleep(.4)
        host.capture("16-service-stopping")
        time.sleep(15.5)
        host.reveal("local_service_retry")
        host.capture("16b-service-no-response")

    def conflict_flow(host):
        open_agent(host)
        host.click("personal_edit")
        host.wait(lambda: host.row("personal_edit_cancel"), "limits editor")
        host.click_text("先问我")
        host.click("personal_review", content=False)
        host.wait(lambda: host.row("personal_submit"), "review")
        host.click("personal_submit", content=False)
        host.wait(lambda: host.row("local_failure_recheck"), "conflict alert")
        time.sleep(.6)
        host.capture("17-save-conflict")
        host.click("local_failure_recheck")
        host.wait(lambda: host.row("personal_submit"), "review again")
        time.sleep(.6)
        host.capture("17b-review-again")

    run("offline", offline_flow, online=False, running=False)
    run("elsewhere", capture_page("13-other-device"), here=False)
    run("error", capture_page("14-status-error"), status_error="configuration_device_unavailable")
    run("empty", empty_flow, empty=True)
    run("pending", pending_flow, hold_service=True)
    run("conflict", conflict_flow, save_error="local_revision_conflict")

    def open_pool(host):
        host.click("agents", content=False)
        host.wait(lambda: host.row("pool_agent_open"), "project agent list")
        host.click("pool_agent_open")
        host.wait(lambda: host.row("pool_agent_back"), "project agent page")
        time.sleep(.8)

    def pool_flow(host):
        open_pool(host)
        host.capture("20-project-agent-top")
        host.scroll(900)
        host.capture("20b-project-agent-bottom")
        host.click("pool_agent_stop")
        host.wait(lambda: host.row("owner_decision_confirm"), "remove alert")
        host.capture("21-project-agent-remove-alert")
        host.click("owner_decision_cancel")
        host.scroll(-100000)
        host.event("ux:dark")
        host.capture("22-project-agent-dark")
        host.event("ux:light")
        host.event("ux:large-text")
        host.capture("23-project-agent-large-text")
        host.scroll(900)
        host.capture("23b-project-agent-large-text-bottom")
        host.scroll(-100000)
        host.click("pool_agent_back", content=False)
        host.wait(lambda: host.row("pool_agent_open"), "back to my agents")

    def capture_pool(name):
        def steps(host):
            open_pool(host)
            host.capture(name)
        return steps

    def owner_flow(host):
        host.click("inbox", content=False)
        host.wait(lambda: host.row("owner_request_open"), "owner inbox")
        host.click("owner_request_open")
        host.wait(lambda: host.row("pool_agent_back"), "request page")
        time.sleep(.8)
        host.capture("26-project-request-owner")
        host.click("owner_approve")
        host.wait(lambda: host.row("owner_decision_confirm"), "approve alert")
        host.capture("26b-project-request-approve-alert")
        host.click("owner_decision_cancel")
        host.click("owner_reject")
        host.wait(lambda: host.row("owner_decision_confirm"), "decline alert")
        host.capture("26c-project-request-decline-alert")
        host.click("owner_decision_cancel")
        host.click("pool_agent_back", content=False)
        host.wait(lambda: host.row("owner_request_open"), "back to the inbox")

    def pools_flow(host):
        host.click("projects", content=False)
        host.wait(lambda: host.row("pool_request"), "pools list")
        time.sleep(.6)
        host.capture("27-pools")
        host.event("ux:dark")
        host.capture("27b-pools-dark")
        host.event("ux:light")
        host.event("ux:large-text")
        host.capture("27c-pools-large-text")

    run("project", pool_flow)
    run("project-capacity", capture_pool("24-project-agent-no-capacity"), pool=dict(
        reservationState="refused", reservationReason="insufficient_capacity", assignmentId=None, membershipState=None))
    run("project-pending", capture_pool("25-project-agent-pending"), pool=dict(
        state="pending_approval", assignmentId=None, membershipState=None, reservationState=None))
    run("pools", pools_flow, pools=[
        dict(id="pool", label="codex-pool-01", ownerMxid="@dspfac:example.test", runtime="codex", revision=7,
             grantRevision=1, publishedTokens=1000000, nativeAvailableTokens=None, canRequest=True, blockingReason=None,
             capabilities=["architect", "coding", "documentation", "integration", "review", "testing"]),
        dict(id="terra", label="terra", ownerMxid="@dspfac:example.test", runtime="codex", revision=2,
             grantRevision=None, publishedTokens=None, nativeAvailableTokens=None, canRequest=False,
             blockingReason="pool_not_shared_with_account", capabilities=["architect", "coding"])])
    run("project-owner", owner_flow, workspace="owner", account="@dspfac:example.test", pool=dict(
        state="pending_approval", assignmentId=None, membershipState=None, reservationState=None,
        canCancel=False, canDecide=True))

if __name__ == "__main__":
    main()
