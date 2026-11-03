import json
import os
import re
import sqlite3
import sys
from contextlib import closing
from pathlib import Path

branch, overview, available_ids, database = sys.argv[1:]
ids = available_ids.split(", ") if available_ids else []
try:
    repos = [line[6:].strip() for line in overview.splitlines() if line.startswith("repo: ")]
    if len(repos) != 1:
        raise ValueError
    repo_path = json.loads(repos[0]) if repos[0].startswith('"') else repos[0]
    if not isinstance(repo_path, str) or not os.path.isabs(repo_path):
        raise ValueError
    with closing(sqlite3.connect(Path(database).as_uri() + "?mode=ro", uri=True, timeout=30)) as db:
        db.execute("BEGIN")
        repo = db.execute("SELECT id FROM repos WHERE working_path = ?", (repo_path,)).fetchall()
        if len(repo) != 1:
            raise ValueError
        rows = db.execute(
            "SELECT id, branch, status, head_sha FROM runs WHERE repo_id = ? AND branch = ? "
            "ORDER BY created_at DESC, id DESC", (repo[0][0], branch)
        ).fetchall()
    displayed_ids = set(ids)
    for row in rows:
        if isinstance(row[0], str) and re.fullmatch(r"[A-Za-z0-9_-]+", row[0]) and row[0] not in ids:
            ids.append(row[0])
    if not displayed_ids.issubset(row[0] for row in rows):
        raise ValueError
    for row in rows:
        if (not all(isinstance(value, str) for value in row)
                or not re.fullmatch(r"[A-Za-z0-9_-]+", row[0]) or row[1] != branch
                or not re.fullmatch(r"[a-z_-]+", row[2]) or not re.fullmatch(r"[a-fA-F0-9]{7,40}", row[3])):
            raise ValueError
    print("count: %d of %d total" % (len(rows), len(rows)))
    print("runs[%d]{id,branch,status,head,pr}:" % len(rows))
    for row in rows:
        print("  " + ",".join(json.dumps(value, ensure_ascii=False) for value in row) + ',""')
except (ValueError, OSError, sqlite3.Error):
    print("unknown|complete same-branch run inventory unreadable; run ids: " + ", ".join(ids))
