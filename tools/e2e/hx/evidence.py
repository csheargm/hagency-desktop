"""Read-only evidence sources: owner ledgers (copied first) and logs."""
import glob
import shutil
import sqlite3
import tempfile
from pathlib import Path

from . import config

LOG_TAIL_BYTES = 4 * 1024 * 1024


def _ledger_for(owner_mxid: str) -> Path | None:
    """The owner ledger whose identity row is this owner (newest wins)."""
    paths = glob.glob(str(config.OWNER_RESOURCES / config.OWNER_LEDGER_GLOB))
    paths = [p for p in paths if "/owner_" in p]
    for path in sorted(paths, key=lambda p: Path(p).stat().st_mtime, reverse=True):
        rows = query_copy(Path(path), "select * from identity")
        if rows and owner_mxid in rows[0]:
            return Path(path)
    return None


def query_copy(path: Path, sql: str, args=()) -> list:
    """Run `sql` on a private copy: the live ledger is never opened."""
    with tempfile.TemporaryDirectory() as tmp:
        copy = Path(tmp) / "ledger.sqlite"
        shutil.copy2(path, copy)
        for suffix in ("-wal", "-shm"):
            if Path(str(path) + suffix).exists():
                shutil.copy2(str(path) + suffix, str(copy) + suffix)
        con = sqlite3.connect(f"file:{copy}?mode=ro", uri=True)
        try:
            return con.execute(sql, args).fetchall()
        finally:
            con.close()


def ledger(owner: str, sql: str, args=()) -> list:
    srv, lp = config.OWNERS[owner]
    path = _ledger_for(config.mxid(lp, srv))
    if path is None:
        raise FileNotFoundError(f"no owner ledger for {owner}")
    return query_copy(path, sql, args)


def agent_id(name: str) -> str:
    """agt_… from the puppet MXID @_hagency_agt_…:server."""
    return config.AGENTS[name][1].split(":")[0].removeprefix("@_hagency_")


def log_since(path: Path, offset: int) -> str:
    """Log text appended after byte `offset` (a log rotated meanwhile is read whole)."""
    if not path.exists():
        return ""
    size = path.stat().st_size
    start = offset if offset <= size else 0
    start = max(start, size - LOG_TAIL_BYTES)
    with open(path, "rb") as f:
        f.seek(start)
        return f.read().decode(errors="replace")


def log_offsets() -> dict:
    paths = {**{f"desktop:{k}": v for k, v in config.DESKTOP_LOGS.items()},
             **{f"server:{k}": v for k, v in config.SERVER_LOGS.items()}}
    return {k: (p, p.stat().st_size if p.exists() else 0) for k, p in paths.items()}


def grep_logs(offsets: dict, needles: tuple) -> list:
    """Lines added since `offsets` that contain any needle: (log, line)."""
    hits = []
    for key, (path, offset) in offsets.items():
        for line in log_since(path, offset).splitlines():
            if any(n in line for n in needles):
                hits.append((key, line[-300:]))
    return hits
