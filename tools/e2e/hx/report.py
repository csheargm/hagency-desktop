"""Markdown report: one table row per scenario, evidence, setup state."""
from collections import Counter
from datetime import datetime

from . import config


def _cell(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ")


def write_report(started: datetime, ctx, results: list, wrong_names: list, cleanup: list):
    counts = Counter(r.status for r in results)
    findings = sum(1 for r in results if r.status == "FAIL" and r.kind == "finding")
    runner = sum(1 for r in results if r.status == "FAIL" and r.kind == "runner")
    summary = (f"e2e {started:%Y-%m-%d %H:%M}: " + ", ".join(f"{k} {v}" for k, v in sorted(counts.items()))
               + f" — {findings} product finding(s), {runner} runner error(s)")
    lines = [f"# Hagency e2e run {started:%Y-%m-%d %H:%M:%S}", "", f"**{summary}**", "",
             f"Room: `{ctx.room}` (\"{config.E2E_ROOM_NAME}\" on alpha). Run id `{ctx.run_id}`.", "",
             "## Setup state", ""]
    lines += [f"- {k}: {v}" for k, v in ctx.setup.items()]
    if wrong_names:
        lines += ["", "Agent name mismatches (config.AGENTS stale):"] + [f"- {w}" for w in wrong_names]
    lines += ["", "## Results", "", "| Scenario | Result | Time | Evidence | Notes |", "|---|---|---|---|---|"]
    for r in results:
        kind = f" ({r.kind})" if r.kind else ""
        lines.append(f"| {_cell(r.name)} | {r.status}{kind} | {r.seconds:.0f}s | "
                     f"{_cell('<br>'.join(map(str, r.evidence)))} | {_cell(r.note)} |")
    lines += ["", "## Cleanup", ""] + ([f"- {c}" for c in cleanup] or ["- nothing to clean"])
    lines += ["", "Legend: FAIL (finding) = product behaviour wrong; FAIL (runner) = harness error; "
              "BLOCKED = precondition missing, never a pass; MANUAL = not automatable yet; "
              "OPT-IN = off by default."]
    config.REPORTS_DIR.mkdir(parents=True, exist_ok=True)
    path = config.REPORTS_DIR / f"{started:%Y%m%d-%H%M%S}.md"
    path.write_text("\n".join(lines) + "\n")
    return path, summary
