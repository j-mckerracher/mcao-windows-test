# DPES-824 test-command wrapper for mutmut 3.8.0 (pytest plugin, scratch only).
# Loaded with: pytest_add_cli_args = ["-p", "dpes824_wrap"] and PYTHONPATH pointing at this directory.
# At session end it writes one run record as JSON under DPES824_RECORD_DIR, outside the mutated
# tree. The record holds the mutant name from MUTANT_UNDER_TEST (set by mutmut in the forked child),
# the run nonce, collected and failed counts, the exit status, and a sentinel string that repeats
# those values. It never changes the exit status. An unwritable record directory leaves no record,
# which the reader treats as reader-error.
import json
import os

import pytest


def _counts(session):
    tr = session.config.pluginmanager.getplugin("terminalreporter")
    failed = 0
    if tr is not None:
        failed = len(tr.stats.get("failed", [])) + len(tr.stats.get("error", []))
    return int(session.testscollected), failed


@pytest.hookimpl(trylast=True)
def pytest_sessionfinish(session, exitstatus):
    collected, failed = _counts(session)
    mutant = os.environ.get("MUTANT_UNDER_TEST", "")
    nonce = os.environ.get("DPES824_NONCE", "")
    rec_dir = os.environ.get("DPES824_RECORD_DIR", "")
    # mutmut 3.8.0 redirects stdout in its fork workers to devnull, so a stdout sentinel never
    # reaches the mutmut log. The sentinel therefore lives in the record file itself.
    sentinel = (
        f"DPES824-SENTINEL nonce={nonce} mutant={mutant} collected={collected} "
        f"failed={failed} exit={int(exitstatus)}"
    )
    record = {
        "nonce": nonce,
        "mutant": mutant,
        "collected": collected,
        "failed": failed,
        "exit": int(exitstatus),
        "pid": os.getpid(),
        "sentinel": sentinel,
    }
    try:
        path = os.path.join(rec_dir, f"{nonce}-{os.getpid()}.json")
        with open(path, "w", encoding="utf-8") as fh:
            json.dump(record, fh, sort_keys=True)
            fh.write("\n")
    except OSError:
        # Unwritable record directory: no record file is left, and the reader treats the mutant as reader-error.
        pass
