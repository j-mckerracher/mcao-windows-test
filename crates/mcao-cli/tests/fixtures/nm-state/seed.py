"""Writes the no-mistakes state database fixtures under the directory given as argv[1].

Each fixture directory `db/<case>/` holds the database the readers open (`state.sqlite`, plus
`state.sqlite-wal` for the live WAL case). The cases are read by
`crates/mcao-cli/src/util/nm_state/tests.rs`, which pins the expected behavior; regenerate
with `MCAO_NM_STATE_REGENERATE=1 cargo test -p mcao-cli nm_state` (see MANIFEST.md).
"""

import os
import shutil
import sqlite3
import sys

REPO_PATH = "/work/repo"
BRANCH = "feat/x"
BASE = 1_700_000_000

CURRENT_TOKENS = [
    "input_tokens", "output_tokens", "cache_read_tokens", "cache_creation_tokens",
]
DELTAS = ["delta_input_tokens", "delta_output_tokens", "delta_cache_read_tokens"]


def create_repos(db, with_table=True):
    if with_table:
        db.execute("CREATE TABLE repos (id TEXT PRIMARY KEY, working_path TEXT NOT NULL UNIQUE)")
        db.execute("INSERT INTO repos VALUES ('repo', ?)", (REPO_PATH,))


def create_runs(db, with_table=True, created_at_type="INTEGER"):
    if with_table:
        db.execute(
            "CREATE TABLE runs (id TEXT PRIMARY KEY, repo_id TEXT NOT NULL, branch TEXT NOT NULL,"
            " status TEXT NOT NULL, head_sha TEXT NOT NULL, created_at %s NOT NULL)" % created_at_type
        )


def create_invocations(db, era="current", with_table=True, drop=()):
    if not with_table:
        return
    columns = ["id", "run_id", "step_name", "round", "purpose", "agent", "session_mode",
               "started_at", "completed_at", "duration_ms", "exit_status"] + CURRENT_TOKENS
    if era == "current":
        columns += ["reasoning_tokens"] + DELTAS
    columns = [c for c in columns if c not in drop]
    db.execute("CREATE TABLE agent_invocations (%s)" % ", ".join(columns))


def insert_run(db, run_id, status, created_at, branch=BRANCH, head="abcdef1234"):
    db.execute("INSERT INTO runs VALUES (?, 'repo', ?, ?, ?, ?)", (run_id, branch, status, head, created_at))


def insert_invocation(db, run_id, n, purpose="review", session_mode="cold", exit_status=0,
                      duration_ms=1000, tokens=(10, 20, 5, 3), deltas=None, started_at=None):
    row = {
        "id": "inv-%s-%03d" % (run_id, n),
        "run_id": run_id,
        "step_name": purpose,
        "round": 1,
        "purpose": purpose,
        "agent": "fake",
        "session_mode": session_mode,
        "started_at": BASE + n if started_at is None else started_at,
        "completed_at": BASE + n,
        "duration_ms": duration_ms,
        "exit_status": exit_status,
        "input_tokens": tokens[0],
        "output_tokens": tokens[1],
        "cache_read_tokens": tokens[2],
        "cache_creation_tokens": tokens[3],
    }
    if deltas is not None:
        row.update(zip(DELTAS, deltas))
        row["reasoning_tokens"] = 0
    available = set(column_names(db))
    row = {k: v for k, v in row.items() if k in available}
    db.execute(
        "INSERT INTO agent_invocations (%s) VALUES (%s)" % (
            ", ".join(row), ", ".join("?" for _ in row)),
        tuple(row.values()),
    )


def column_names(db):
    return [r[1] for r in db.execute("PRAGMA table_info(agent_invocations)")]


def standard(db, era="current", runs=3):
    create_repos(db)
    create_runs(db)
    create_invocations(db, era)
    insert_run(db, "run-old", "completed", BASE - 500)
    insert_run(db, "run-mid", "failed", BASE + 10)
    insert_run(db, "run-new", "completed", BASE + 100)
    insert_invocation(db, "run-old", 1, "review", "cold", 0, 500, (10, 20, 5, 3),
                      (10, 20, 5) if era == "current" else None)
    insert_invocation(db, "run-mid", 2, "review", "resumed", 1, 700, (50, 60, 9, 7),
                      (12, 20, 5) if era == "current" else None)
    insert_invocation(db, "run-mid", 3, "review-fix", "cold", 0, 300, (7, 8, 0, 1),
                      (7, 8, 0) if era == "current" else None)
    insert_invocation(db, "run-new", 4, "test", "resumed", "cancelled", 2000, (100, 200, 30, 40),
                      (100, 200, 30) if era == "current" else None)
    insert_invocation(db, "run-new", 5, "ci", "cold", 0, 0, (None, None, None, None),
                      (None, None, None) if era == "current" else None)


def fixture_path(case):
    directory = os.path.join(OUT, "db", case)
    os.makedirs(directory, exist_ok=True)
    return os.path.join(directory, "state.sqlite")


def clear(path):
    for leftover in (path, path + "-wal", path + "-shm", path + "-journal"):
        if os.path.exists(leftover):
            os.remove(leftover)


def write_db(case, build, wal=False):
    path = fixture_path(case)
    clear(path)
    db = sqlite3.connect(path)
    if wal:
        db.execute("PRAGMA journal_mode=WAL")
    build(db)
    db.commit()
    db.close()


def build_wal_live(case):
    """A WAL database whose newest rows exist only in the -wal file.

    The writer keeps the database open while the two files are copied into the fixture, so the
    copy holds the uncheckpointed frames and no -shm, which SQLite rebuilds on open.
    """
    scratch = os.path.join(OUT, "scratch", case)
    os.makedirs(scratch, exist_ok=True)
    source = os.path.join(scratch, "state.sqlite")
    clear(source)
    db = sqlite3.connect(source)
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA wal_autocheckpoint=0")
    standard(db)
    db.commit()
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    insert_run(db, "run-live", "running", BASE + 200)
    insert_invocation(db, "run-live", 6, "review", "cold", 0, 900, (1, 2, 3, 4), (1, 2, 3))
    db.commit()
    target = fixture_path(case)
    clear(target)
    shutil.copyfile(source, target)
    shutil.copyfile(source + "-wal", target + "-wal")
    db.close()
    shutil.rmtree(os.path.join(OUT, "scratch"))


OUT = ""


def main():
    global OUT
    OUT = sys.argv[1]

    write_db("wal-at-rest", standard, wal=True)
    build_wal_live("wal-live")
    write_db("rollback-journal", standard)
    write_db("pre-delta-era", lambda db: standard(db, era="pre-delta"))

    def no_repos(db):
        create_repos(db, with_table=False)
        create_runs(db)
        create_invocations(db)
    write_db("missing-repos-table", no_repos)

    def no_runs(db):
        create_repos(db)
        create_runs(db, with_table=False)
        create_invocations(db)
    write_db("missing-runs-table", no_runs)

    def no_invocations(db):
        create_repos(db)
        create_runs(db)
        insert_run(db, "run-a", "completed", BASE)
        create_invocations(db, with_table=False)
    write_db("missing-agent-invocations-table", no_invocations)

    def empty_runs(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
    write_db("empty-runs", empty_runs)

    def zero_row_repo(db):
        db.execute("CREATE TABLE repos (id TEXT PRIMARY KEY, working_path TEXT NOT NULL UNIQUE)")
        db.execute("INSERT INTO repos VALUES ('other', '/work/other')")
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-a", "completed", BASE)
    write_db("zero-row-repo", zero_row_repo)

    def many_runs(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        for n in range(60):
            run_id = "run-%03d" % n
            insert_run(db, run_id, "completed" if n % 3 else "failed", BASE + n)
            insert_invocation(db, run_id, n, "review" if n % 2 else "test", "cold", 0, n, (n, n, n, n),
                              (n, n, n))
    write_db("many-runs", many_runs)

    def created_at_ties(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        # Four runs share created_at, inserted out of id order, so the inventory's
        # ORDER BY created_at DESC, id DESC is the only thing that fixes their sequence.
        for n, run_id in enumerate(("run-t3", "run-t1", "run-t4", "run-t2"), start=1):
            insert_run(db, run_id, "completed", BASE)
            insert_invocation(db, run_id, n, "review" if n % 2 else "test", "cold", 0, 10 * n,
                              (n, 1, 1, 1), (n, 1, 1))
        insert_run(db, "run-t5", "failed", BASE + 1)
        insert_invocation(db, "run-t5", 5, "review", "cold", 1, 10, (1, 2, 3, 4), (1, 2, 3))
    write_db("created-at-ties", created_at_ties)

    def unicode_names(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        branch = "feat/ünï-🚀-漢字"
        insert_run(db, "run-u1", "completed", BASE, branch=branch)
        insert_run(db, "run-u2", "failed", BASE + 1, branch=branch)
        insert_invocation(db, "run-u1", 1, "review-漢字", "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
        insert_invocation(db, "run-u2", 2, "test🙂", "cold", 0, 20, (5, 6, 7, 8), (5, 6, 7))
    write_db("unicode-names", unicode_names)

    def control_chars(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        branch = "feat/\t\x01\x7f\x08\x0c\r\n\"\\x"
        insert_run(db, "run-c1", "completed", BASE, branch=branch)
        insert_invocation(db, "run-c1", 1, "a\x08\x0c\n\r\t\x1f\x7f\"\\b", "cold", 0, 10, (1, 2, 3, 4),
                          (1, 2, 3))
    write_db("control-chars", control_chars)

    def nul_bytes(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-n1", "completed", BASE)
        insert_invocation(db, "run-n1", 1, "a\x00b", "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
    write_db("nul-bytes", nul_bytes)

    def token_values(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-t1", "completed", BASE)
        big = 9223372036854775807
        insert_invocation(db, "run-t1", 1, "review", "cold", 0, big, (big, big, big, big), (big, big, big))
        insert_invocation(db, "run-t1", 2, "review", "cold", 0, -5, (-7, None, 2, -1), (-7, None, 2))
        insert_invocation(db, "run-t1", 3, "review", "cold", 0, 2.5, (1.5, 2.0, 0.0, 1e16), (1.5, 2.0, 0.0))
        insert_invocation(db, "run-t1", 4, "review", "cold", 0, 1e-5, (None, 3, 4, None), (None, 3, 4))
    write_db("token-values", token_values)

    def huge_sums(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-h1", "completed", BASE)
        big = 9223372036854775807
        for n in range(3):
            insert_invocation(db, "run-h1", n, "review", "cold", 0, big, (big, big, big, big), (big, big, big))
    write_db("huge-sums", huge_sums)

    def float_duration(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-f1", "completed", BASE)
        insert_invocation(db, "run-f1", 1, "review", "cold", 0, 0.0, (1, 1, 1, 1), (1, 1, 1))
        insert_invocation(db, "run-f1", 2, "review", "cold", 0, 1e16, (1, 1, 1, 1), (1, 1, 1))
        insert_invocation(db, "run-f1", 3, "review", "cold", 0, 0.1, (1, 1, 1, 1), (1, 1, 1))
        insert_invocation(db, "run-f1", 4, "review", "cold", 0, -0.0, (1, 1, 1, 1), (1, 1, 1))
        insert_invocation(db, "run-f1", 5, "review", "cold", 0, "", (1, 1, 1, 1), (1, 1, 1))
    write_db("float-duration", float_duration)

    def text_token(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-x1", "completed", BASE)
        insert_invocation(db, "run-x1", 1, "review", "cold", 0, 10, ("12", 2, 3, 4), (1, 2, 3))
    write_db("text-token", text_token)

    def blob_token(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-b1", "completed", BASE)
        insert_invocation(db, "run-b1", 1, "review", "cold", 0, 10, (1, b"\x01", 3, 4), (1, 2, 3))
    write_db("blob-token", blob_token)

    def invalid_utf8_status(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-s1", "completed", BASE)
        db.execute("UPDATE runs SET status = CAST(X'636F6DFF' AS TEXT) WHERE id = 'run-s1'")
    write_db("invalid-utf8-status", invalid_utf8_status)

    def invalid_utf8_purpose(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-p1", "completed", BASE)
        insert_invocation(db, "run-p1", 1, "x", "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
        db.execute("UPDATE agent_invocations SET purpose = CAST(X'7866FFC3' AS TEXT) WHERE id = 'inv-run-p1-001'")
    write_db("invalid-utf8-purpose", invalid_utf8_purpose)

    def created_at_real(db):
        create_repos(db)
        create_runs(db, created_at_type="REAL")
        create_invocations(db)
        insert_run(db, "run-r1", "completed", 1234.5)
        insert_run(db, "run-r2", "completed", -1.5)
        insert_invocation(db, "run-r1", 1, "review", "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
        insert_invocation(db, "run-r2", 2, "review", "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
    write_db("created-at-real", created_at_real)

    def created_at_text(db):
        create_repos(db)
        create_runs(db, created_at_type="TEXT")
        create_invocations(db)
        insert_run(db, "run-tx", "completed", "2020-01-01")
    write_db("created-at-text", created_at_text)

    def created_at_overflow(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-o1", "completed", 100000000000000000)
        insert_invocation(db, "run-o1", 1, "review", "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
    write_db("created-at-overflow", created_at_overflow)

    def purposes_mixed(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-m1", "completed", BASE)
        insert_invocation(db, "run-m1", 1, None, "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
        insert_invocation(db, "run-m1", 2, "review", "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
    write_db("purposes-mixed", purposes_mixed)

    def purpose_null(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-n2", "completed", BASE)
        insert_invocation(db, "run-n2", 1, None, "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
    write_db("purpose-null-only", purpose_null)

    def purpose_numeric(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-q1", "completed", BASE)
        insert_invocation(db, "run-q1", 1, 1.5, "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
        insert_invocation(db, "run-q1", 2, 1, "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
        insert_invocation(db, "run-q1", 3, 1.0, "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
        insert_invocation(db, "run-q1", 4, 0.5, "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
    write_db("purpose-numeric", purpose_numeric)

    def exit_types(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-e1", "completed", BASE)
        insert_invocation(db, "run-e1", 1, "review", "cold", 1, 10, (1, 2, 3, 4), (1, 2, 3))
        insert_invocation(db, "run-e1", 2, "review", "cold", "1", 10, (1, 2, 3, 4), (1, 2, 3))
        insert_invocation(db, "run-e1", 3, "review", "cold", 1.0, 10, (1, 2, 3, 4), (1, 2, 3))
        insert_invocation(db, "run-e1", 4, "review", "cold", None, 10, (1, 2, 3, 4), (1, 2, 3))
        insert_invocation(db, "run-e1", 5, "review", "cold", float("inf"), 10, (1, 2, 3, 4), (1, 2, 3))
    write_db("exit-types", exit_types)

    def resumed(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-z1", "completed", BASE)
        insert_invocation(db, "run-z1", 1, "review", "resumed", 0, 10, (5, 6, 7, 8), (5, 6, 7))
        insert_invocation(db, "run-z1", 2, "review", "resumed", 0, 10, (9, 9, 9, 9), (5, 6, 7))
        insert_invocation(db, "run-z1", 3, "review", "resumed", 0, 10, (9, 9, 9, 9), None)
        insert_invocation(db, "run-z1", 4, "review", "cold", 0, 10, (9, 9, 9, 9), None)
        insert_invocation(db, "run-z1", 5, "review", "resumed", 0, 10, (9, 9, 9, 9), (9, 9, 9))
    write_db("resumed-sessions", resumed)

    def missing_started_at(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db, drop=("started_at",))
        insert_run(db, "run-l1", "completed", BASE)
    write_db("missing-started-at", missing_started_at)

    def since_boundary(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-before", "completed", BASE - 1)
        insert_run(db, "run-at", "completed", BASE)
        insert_run(db, "run-after", "completed", BASE + 1)
        for run_id in ("run-before", "run-at", "run-after"):
            insert_invocation(db, run_id, 1, "review", "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
    write_db("since-boundary", since_boundary)

    def pre_delta_no_reasoning(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db, era="pre-delta")
        insert_run(db, "run-d1", "completed", BASE)
        insert_invocation(db, "run-d1", 1, "review", "resumed", 0, 10, (1, 2, 3, 4))
        insert_invocation(db, "run-d1", 2, "review", "cold", 0, 10, (1, 2, 3, 4))
    write_db("pre-delta-no-deltas", pre_delta_no_reasoning)

    def bad_head(db):
        create_repos(db)
        create_runs(db)
        create_invocations(db)
        insert_run(db, "run-hd1", "completed", BASE, head="zz-not-a-sha")
        insert_run(db, "run-hd2", "completed", BASE + 1)
        insert_invocation(db, "run-hd2", 1, "review", "cold", 0, 10, (1, 2, 3, 4), (1, 2, 3))
    write_db("bad-head", bad_head)

    with open(fixture_path("not-sqlite"), "w", encoding="utf-8") as handle:
        handle.write("this is not a sqlite database\n")

    print("wrote fixtures under", OUT)


if __name__ == "__main__":
    main()
