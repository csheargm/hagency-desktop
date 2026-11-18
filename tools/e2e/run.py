#!/usr/bin/env python3
"""Hagency local-demo end-to-end scenario runner.

Usage: python3 run.py [--only 1,2,7] [--inject pause-betty] [--no-cleanup]
Writes reports/<timestamp>.md and prints a one-line summary. Exit 1 on any FAIL."""
import argparse
import sys
import time
import traceback
from datetime import datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from hx import config, github, owner_ui, room  # noqa: E402
from hx.bridge import BridgeDown, StepNotFound  # noqa: E402
from hx.report import write_report  # noqa: E402
from hx.scenario import BLOCKED, FAIL, Ctx, Result  # noqa: E402
from scenarios import approvals, chat, issues, manual  # noqa: E402


def scenario_table(args):
    inject = args.inject == "pause-betty"
    return [
        ("1", chat.s1_mention_answer), ("2", chat.s2_cross_server), ("3", chat.s3_thread_followup),
        ("4", chat.s4_room_mention), ("5", approvals.s5_readonly_query),
        ("6", approvals.s6_write_needs_approval), ("7", issues.s7_issue_loop),
        ("8", issues.s8_cross_model_review), ("9", lambda c: manual.s9_failure_injection(c, inject)),
        ("10", manual.s10_agent_lifecycle),
    ]


def run_one(key, fn, ctx) -> Result:
    t0 = time.time()
    try:
        return fn(ctx)
    except Exception as e:  # noqa: BLE001 — reported as a runner error with its traceback, never swallowed
        tb = traceback.format_exc().strip().splitlines()[-3:]
        return Result(f"{key} ({fn.__name__ if hasattr(fn, '__name__') else key})", FAIL,
                      time.time() - t0, tb, f"runner error: {e}", kind="runner")


def preflight(ui_setup: bool) -> tuple:
    if github.whoami() != github.GH_ACCOUNT:
        raise SystemExit(f"gh does not act as {github.GH_ACCOUNT}")
    ctx_room = room.ensure_room()
    log = owner_setup(ctx_room) if ui_setup else ["owner UI setup skipped (--no-ui-setup)"]
    return ctx_room, room.setup_status(ctx_room), room.verify_agent_names(), log


def owner_setup(room_id: str) -> list:
    """One-time owner steps through each window's bridge; skipped per owner
    when the owner and all their Agents are already in the room."""
    log = []
    status = room.setup_status(room_id)
    for owner in config.OWNERS:
        agents = [a for a, (o, _) in config.AGENTS.items() if o == owner]
        keys = [f"owner:{owner}"] + [f"agent:{a}" for a in agents]
        if all(status.get(k) == "join" for k in keys):
            log.append(f"{owner}: setup already done")
            continue
        try:
            log += owner_ui.setup_owner(owner, room_id)
        except (BridgeDown, StepNotFound) as e:
            log.append(f"{owner}: setup incomplete: {e}")
    return log


def run_all(args, ctx) -> list:
    wanted = set(filter(None, args.only.split(",")))
    table = scenario_table(args)
    unknown = wanted - {k for k, _ in table}
    if unknown:
        raise SystemExit(f"unknown scenario(s): {sorted(unknown)}")
    results = []
    for key, fn in table:
        if wanted and key not in wanted:
            continue
        print(f"[{datetime.now():%H:%M:%S}] scenario {key} …", flush=True)
        res = run_one(key, fn, ctx)
        print(f"[{datetime.now():%H:%M:%S}]   {res.status} ({res.seconds:.0f}s) {res.note[:160]}", flush=True)
        results.append(res)
    return results


def exit_code(results: list) -> int:
    """1 on any FAIL; 2 when nothing was verified (none ran, or any BLOCKED); else 0."""
    if any(r.status == FAIL for r in results):
        return 1
    if not results or any(r.status == BLOCKED for r in results):
        return 2
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", default="")
    ap.add_argument("--inject", default="", choices=["", "pause-betty"])
    ap.add_argument("--no-cleanup", action="store_true")
    ap.add_argument("--no-ui-setup", action="store_true", help="do not drive the owners' windows")
    args = ap.parse_args()
    started = datetime.now()
    e2e_room, setup, wrong, setup_log = preflight(not args.no_ui_setup)
    ctx = Ctx(room=e2e_room, setup=setup, run_id=started.strftime("%m%d%H%M%S"), setup_log=setup_log)
    for line in setup_log:
        print(f"setup: {line}", flush=True)
    results, cleanup = [], []
    try:
        results = run_all(args, ctx)
    finally:  # an interrupt still cleans up and still writes the report
        cleanup = ["skipped (--no-cleanup)"] if args.no_cleanup else issues.cleanup(ctx)
        path, summary = write_report(started, ctx, results, wrong, cleanup)
        print(summary)
        print(f"report: {path}")
    return exit_code(results)


if __name__ == "__main__":
    sys.exit(main())
