import fcntl
import json
import os
import sqlite3
import sys
import time
from contextlib import closing
from pathlib import Path

ledger, task, spawn_gen, repo, branch, since, database, reason = sys.argv[1:]
COUNTERS = ("input", "output", "cache_read")
TOKENS = tuple(f + "_tokens" for f in COUNTERS) + ("cache_creation_tokens",)


def utc(epoch):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(epoch))


def tally():
    t = {"invocations": 0, "exit": {}, "duration_ms": 0}
    for field in TOKENS:
        t[field] = {"total": 0, "unknown": 0}
    return t


def add(t, row, tokens):
    t["invocations"] += 1
    t["exit"][row["exit_status"]] = t["exit"].get(row["exit_status"], 0) + 1
    t["duration_ms"] += row["duration_ms"] or 0
    for field in TOKENS:
        if tokens[field] is None:
            t[field]["unknown"] += 1
        else:
            t[field]["total"] += tokens[field]


def invocation_tokens(row):
    resumed = row["session_mode"] == "resumed"
    proven = True  # every per-round delta recorded and equal to its raw counter
    tokens = {}
    for counter in COUNTERS:
        raw, delta = row[counter + "_tokens"], row["delta_" + counter + "_tokens"]
        if delta is None:
            proven = False
            tokens[counter + "_tokens"] = None if resumed else raw
        else:
            proven = proven and raw == delta
            tokens[counter + "_tokens"] = delta
    per_invocation = not resumed or proven
    tokens["cache_creation_tokens"] = row["cache_creation_tokens"] if per_invocation else None
    return tokens


def read_spend():
    path = Path(database)
    if not path.is_absolute():
        raise ValueError("state database path %s is not absolute" % database)
    with closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=30)) as db:
        db.row_factory = sqlite3.Row
        db.execute("BEGIN")
        columns = {r["name"] for r in db.execute("PRAGMA table_info(agent_invocations)")}
        if not columns:
            raise ValueError("no agent_invocations table")
        repo_rows = db.execute("SELECT id FROM repos WHERE working_path = ?", (repo,)).fetchall()
        if len(repo_rows) != 1:
            raise ValueError("no repository %s" % repo)
        runs = db.execute(
            "SELECT id, status, created_at FROM runs WHERE repo_id = ? AND branch = ? AND created_at >= ? "
            "ORDER BY created_at, id",
            (repo_rows[0]["id"], branch, int(since) if since else 0),
        ).fetchall()
        wanted = ("run_id", "purpose", "session_mode", "exit_status", "duration_ms") + TOKENS + tuple(
            "delta_" + c + "_tokens" for c in COUNTERS
        )
        select = ", ".join(c if c in columns else "NULL AS " + c for c in wanted)
        invocations = []
        for run in runs:
            invocations.extend(db.execute(
                "SELECT %s FROM agent_invocations WHERE run_id = ? ORDER BY started_at, id" % select,
                (run["id"],),
            ).fetchall())
    total = tally()
    per_run = {run["id"]: tally() for run in runs}
    per_purpose = {}
    for row in invocations:
        tokens = invocation_tokens(row)
        add(total, row, tokens)
        add(per_run[row["run_id"]], row, tokens)
        add(per_purpose.setdefault(row["purpose"], tally()), row, tokens)
    return {
        "total": total,
        "runs": [
            dict({"id": run["id"], "status": run["status"], "created_at": utc(run["created_at"])}, **per_run[run["id"]])
            for run in runs
        ],
        "purposes": [dict({"purpose": name}, **per_purpose[name]) for name in sorted(per_purpose)],
    }


record = {
    "task": task,
    "spawn_gen": spawn_gen or None,
    "recorded_at": utc(time.time()),
    "source": "no-mistakes-state",
    "reason": None,
    "repo": repo or None,
    "branch": branch or None,
    "since": utc(int(since)) if since else None,
}
if not reason:
    try:
        spend = read_spend()
    except (ValueError, OSError, sqlite3.Error) as err:
        reason = "cannot read no-mistakes state: %s" % err
if reason:
    record.update(source="unavailable", reason=reason, total=None, runs=[], purposes=[])
else:
    record.update(spend)
line = json.dumps(record, separators=(",", ":"))

try:
    fd = os.open(ledger, os.O_RDWR | os.O_CREAT | os.O_APPEND | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "r+", encoding="utf-8") as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        content = f.read()
        for existing in content.splitlines():
            try:
                row = json.loads(existing)
            except ValueError:
                continue
            if isinstance(row, dict) and row.get("task") == task and row.get("spawn_gen") == record["spawn_gen"]:
                print("already recorded %s %s" % (task, spawn_gen or "-"))
                sys.exit(0)
        f.write(("\n" if content and not content.endswith("\n") else "") + line + "\n")
        f.flush()
        os.fsync(f.fileno())
except OSError as err:
    sys.stderr.write("fm-pipeline-spend: cannot write %s: %s\n" % (ledger, err))
    sys.exit(1)
print("recorded %s %s" % (task, spawn_gen or "-"))
