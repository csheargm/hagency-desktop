"""Fixed facts about the local Hagency demo the scenarios run against.

Every value that more than one module needs lives here (R3)."""
import os
from pathlib import Path

ROOT = Path(os.environ.get("HAGENCY_ROOT", "/Users/lirongxie/claudeworkfolder/hagency"))
SECRETS = ROOT / "hagency-server" / "secrets"
RUN = ROOT / "hagency-server" / ".run"
E2E_DIR = Path(__file__).resolve().parent.parent
STATE_DIR = Path(os.environ.get("HAGENCY_E2E_STATE", str(RUN / "e2e")))
REPORTS_DIR = E2E_DIR / "reports"

SERVERS = {
    "alpha": {"http": "http://127.0.0.1:8090", "name": "localhost:8450"},
    "beta": {"http": "http://127.0.0.1:8092", "name": "localhost:8452"},
}
FORBIDDEN_PORTS = ("8088",)

# Throwaway people the runner owns: localpart -> server.
PERSONAS = {"zz-anna": "alpha", "zz-betty": "beta", "zz-will": "beta"}
# Demo owners: name -> (server, localpart). The runner never signs in as them:
# their Agents serve a room only after the owner binds it in their own window.
OWNERS = {"betty": ("beta", "bird"), "anna": ("alpha", "csheargm")}

# Agent name -> (owner, puppet MXID). The runner re-checks each display name.
AGENTS = {
    "anna-claude": ("anna", "@_hagency_agt_01m7k5g5n36gcamvq8f8m07e6n:localhost:8450"),
    "yue-claude": ("anna", "@_hagency_agt_01m7kd31m0vbgw7ed27chayat9:localhost:8450"),
    "bird-claude": ("betty", "@_hagency_agt_01m7k84kgg5a5mtm2epqhbpjb5:localhost:8452"),
    "bird-codex": ("betty", "@_hagency_agt_01m7k6ws4k2z06ysr6shrxeqzf:localhost:8452"),
}
DEV_ROOM = "!a3cQXHR8GZ4RP5n71s:localhost:8450"  # Chris's room: read only, never written
E2E_ROOM_NAME = "e2e room"
SANDBOX_REPO = "csheargm/hagency-demo-sandbox"
TEST_MARK = "[e2e]"  # every issue/PR/branch the runner creates carries this

DESKTOP_LOGS = {"anna": RUN / "fresh-anna" / "desktop.log", "betty": RUN / "fresh-betty" / "desktop.log"}
SERVER_LOGS = {"alpha": RUN / "fed" / "server-alpha.log", "beta": RUN / "fed" / "server-beta.log"}
OWNER_LEDGER_GLOB = "owners/*/owned-agent-owners/*/agent-local.sqlite"
OWNER_RESOURCES = Path.home() / ".hagency-owner-resources"


def mxid(localpart: str, server: str) -> str:
    return f"@{localpart}:{SERVERS[server]['name']}"


def password_file(localpart: str) -> Path:
    return SECRETS / f"e2e-{localpart}.password"
