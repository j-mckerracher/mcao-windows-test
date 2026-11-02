#!/usr/bin/env python3
# Owned stand-in for an agent session, used only by the non-interference proof (AC-17, T10).
# It is not a live agent. It keeps a heartbeat counter current and appends the number of each
# signal it receives to a record file, so a test can see whether this process kept running and
# whether any signal reached it.
import os
import signal
import sys
import time

beat_path, signal_path = sys.argv[1], sys.argv[2]

# Own session: a signal sent to the observer's process group cannot reach this process.
os.setsid()


def record(signum, _frame):
    with open(signal_path, "a", encoding="utf-8") as record_file:
        record_file.write(f"{signum}\n")


for name in ("SIGTERM", "SIGINT", "SIGHUP", "SIGTSTP", "SIGUSR1"):
    signal.signal(getattr(signal, name), record)

beat = 0
while True:
    beat += 1
    tmp_path = beat_path + ".tmp"
    with open(tmp_path, "w", encoding="utf-8") as beat_file:
        beat_file.write(str(beat))
    os.replace(tmp_path, beat_path)
    time.sleep(0.05)
