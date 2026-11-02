"""Trusted supervision: process-group control, heartbeats, checkpoints, bounded flush, redaction.

DPES-811 AC5. These are real subprocess tests with short injected intervals. The production
defaults (15 s heartbeat, 300 s checkpoint cadence) are asserted directly rather than waited for.
"""

import itertools
import json
import os
import signal
import sys
import threading
import time
from pathlib import Path

import pytest

from mao_cloud import trusted_supervise

from mao_cloud.protocol import MAX_LOG_LINE_CHARS, redact
from mao_cloud.trusted_supervise import (
    ChildOutcome,
    OutputLine,
    SupervisorTiming,
    checkpoint_due,
    supervise,
)

PY = sys.executable
ENV = {"PATH": os.environ.get("PATH", "/usr/bin:/bin")}
FAST = SupervisorTiming(
    heartbeat_seconds=0.05,
    checkpoint_min_interval_seconds=0.2,
    flush_grace_seconds=0.5,
)


class Collector:
    def __init__(self) -> None:
        self.lines: list[str] = []
        self.beats = 0

    def sink(self, line: str) -> None:
        self.lines.append(line)

    def heartbeat(self) -> None:
        self.beats += 1


def _supervise(argv, *, max_runtime=30.0, timing=FAST, **kwargs):
    collector = Collector()
    result = supervise(
        argv,
        env=ENV,
        max_runtime_seconds=max_runtime,
        sink=collector.sink,
        heartbeat=collector.heartbeat,
        timing=timing,
        **kwargs,
    )
    return result, collector


def _gone_soon(pid: int, seconds: float = 5.0) -> bool:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        time.sleep(0.05)
    return False


# --- exit classification: never success unless a clean zero exit ----------------------------


def test_a_zero_exit_is_completed_and_its_output_reaches_the_sink() -> None:
    result, out = _supervise([PY, "-c", "print('hello')"])
    assert result.outcome is ChildOutcome.COMPLETED
    assert result.returncode == 0
    assert out.lines == ["hello"]


def test_a_nonzero_exit_is_failed_never_completed() -> None:
    result, _ = _supervise([PY, "-c", "import sys; sys.exit(3)"])
    assert result.outcome is ChildOutcome.FAILED
    assert result.returncode == 3


def test_a_child_killed_by_a_signal_is_killed_never_completed() -> None:
    """An out-of-memory kill arrives as a signal. It must never be reported as success."""
    code = "import os, signal; os.kill(os.getpid(), signal.SIGKILL)"
    result, _ = _supervise([PY, "-c", code])
    assert result.outcome is ChildOutcome.KILLED
    assert result.returncode == -signal.SIGKILL


def test_the_child_leads_its_own_process_group() -> None:
    _, out = _supervise([PY, "-c", "import os; print(os.getpgid(0) == os.getpid())"])
    assert out.lines == ["True"]


# --- output: redacted and capped before it reaches the sink ---------------------------------


def test_output_is_redacted_before_the_sink_receives_it() -> None:
    secret_line = "token=hunter2 and Bearer abc.def-ghi"
    _, out = _supervise([PY, "-c", f"print({secret_line!r})"])
    assert out.lines == [redact(secret_line).text]
    assert "hunter2" not in out.lines[0]
    assert "abc.def-ghi" not in out.lines[0]


def test_a_long_line_is_capped_at_the_protocol_limit() -> None:
    _, out = _supervise([PY, "-c", "print('x' * 10000)"])
    assert len(out.lines) == 1
    assert len(out.lines[0]) <= MAX_LOG_LINE_CHARS
    assert out.lines[0].startswith("x")


def test_a_raised_line_limit_lets_a_structured_event_arrive_whole() -> None:
    event = "z" * 10000
    _, out = _supervise([PY, "-c", f"print({event!r})"], line_limit=20000)
    assert out.lines == [event]


def test_a_line_with_no_newline_cannot_grow_the_buffer_and_later_lines_still_arrive() -> None:
    code = "import sys; sys.stdout.write('y' * 1000000 + '\\n'); print('after', flush=True)"
    _, out = _supervise([PY, "-c", code])
    assert len(out.lines) == 2
    assert len(out.lines[0]) <= MAX_LOG_LINE_CHARS
    assert out.lines[1] == "after"


# --- heartbeats -----------------------------------------------------------------------------


def test_heartbeats_arrive_on_the_configured_interval() -> None:
    result, out = _supervise([PY, "-c", "import time; time.sleep(0.4)"])
    assert result.outcome is ChildOutcome.COMPLETED
    assert out.beats >= 3


def test_the_production_heartbeat_is_fifteen_seconds() -> None:
    assert SupervisorTiming().heartbeat_seconds == 15


def test_the_production_checkpoint_cadence_is_at_most_every_five_minutes() -> None:
    assert SupervisorTiming().checkpoint_min_interval_seconds == 300


# --- checkpoints: only on change, never more often than the interval ------------------------


def test_checkpoint_is_due_only_when_changed_and_the_interval_has_passed() -> None:
    assert checkpoint_due(last=None, now=0.0, changed=False, min_interval=300) is False
    assert checkpoint_due(last=None, now=0.0, changed=True, min_interval=300) is True
    assert checkpoint_due(last=0.0, now=299.9, changed=True, min_interval=300) is False
    assert checkpoint_due(last=0.0, now=300.0, changed=True, min_interval=300) is True


def test_an_unchanged_workspace_never_checkpoints() -> None:
    checkpoints: list[float] = []
    _supervise(
        [PY, "-c", "import time; time.sleep(0.3)"],
        fingerprint=lambda: "same",
        checkpoint=lambda: checkpoints.append(time.monotonic()),
    )
    assert checkpoints == []


def test_checkpoints_follow_changes_and_never_come_closer_than_the_interval() -> None:
    stamps: list[float] = []
    counter = itertools.count()
    _supervise(
        [PY, "-c", "import time; time.sleep(0.8)"],
        fingerprint=lambda: str(next(counter)),
        checkpoint=lambda: stamps.append(time.monotonic()),
    )
    assert len(stamps) >= 2
    gaps = [later - earlier for earlier, later in zip(stamps, stamps[1:], strict=False)]
    assert all(gap >= FAST.checkpoint_min_interval_seconds - 0.02 for gap in gaps)


def test_the_production_checkpoint_is_not_taken_without_a_change_source() -> None:
    result, _ = _supervise([PY, "-c", "print('no checkpoints wired')"])
    assert result.outcome is ChildOutcome.COMPLETED


# --- deadline: bounded SIGTERM flush, then SIGKILL, never success ---------------------------


def test_the_deadline_flushes_with_sigterm_and_reports_timed_out_not_completed() -> None:
    code = (
        "import signal, sys, time\n"
        "def stop(signum, frame):\n"
        "    print('flushed', flush=True)\n"
        "    sys.exit(0)\n"
        "signal.signal(signal.SIGTERM, stop)\n"
        "print('ready', flush=True)\n"
        "time.sleep(60)\n"
    )
    result, out = _supervise([PY, "-c", code], max_runtime=1.0)
    assert result.outcome is ChildOutcome.TIMED_OUT
    assert "flushed" in out.lines


def test_a_child_that_ignores_sigterm_is_killed_after_the_grace() -> None:
    code = "import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)"
    started = time.monotonic()
    result, _ = _supervise([PY, "-c", code], max_runtime=0.5)
    assert result.outcome is ChildOutcome.TIMED_OUT
    assert time.monotonic() - started < 10


def test_the_whole_process_group_is_stopped_including_grandchildren(tmp_path: Path) -> None:
    pid_file = tmp_path / "grandchild.pid"
    code = (
        "import subprocess, sys, time, pathlib\n"
        "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\n"
        f"pathlib.Path({str(pid_file)!r}).write_text(str(child.pid))\n"
        "time.sleep(60)\n"
    )
    result, _ = _supervise([PY, "-c", code], max_runtime=1.5)
    assert result.outcome is ChildOutcome.TIMED_OUT
    assert _gone_soon(int(pid_file.read_text()))


@pytest.mark.parametrize("terminated", [True, False])
def test_pem_private_key_body_is_suppressed_across_raw_chunks(monkeypatch, terminated) -> None:
    monkeypatch.setattr(trusted_supervise, "READ_CHUNK_BYTES", 3)
    key = "before\n-----BEGIN RSA PRIVATE KEY-----\nprivate-body-one\nprivate-body-two\n"  # gitleaks:allow - synthetic key body for a redaction test, not a credential
    if terminated:
        key += "-----END RSA PRIVATE KEY-----\nafter\n"
    result, out = _supervise([PY, "-c", f"import sys; sys.stdout.write({key!r})"])
    text = "\n".join(out.lines)
    assert result.outcome is ChildOutcome.COMPLETED
    assert "before" in text
    assert "[REDACTED]" in text
    assert "PRIVATE KEY" not in text
    assert "private-body-one" not in text
    assert "private-body-two" not in text
    assert ("after" in text) is terminated


def test_pem_marker_after_a_truncated_line_still_suppresses_following_body(monkeypatch) -> None:
    monkeypatch.setattr(trusted_supervise, "READ_CHUNK_BYTES", 17)
    key = (
        "x" * (MAX_LOG_LINE_CHARS + 100)
        + "-----BEGIN OPENSSH PRIVATE KEY-----\nprivate-secret-body\n"
        + "-----END OPENSSH PRIVATE KEY-----\nafter\n"
    )
    _, out = _supervise([PY, "-c", f"import sys; sys.stdout.write({key!r})"])
    text = "\n".join(out.lines)
    assert "private-secret-body" not in text
    assert "PRIVATE KEY" not in text
    assert out.lines[-1] == "after"
    assert all(len(line) <= MAX_LOG_LINE_CHARS for line in out.lines)


def test_full_output_queue_does_not_leave_a_reader_or_child_after_sink_failure(
    monkeypatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(trusted_supervise, "OUTPUT_QUEUE_LINES", 1)
    pid_file = tmp_path / "child.pid"
    code = (
        "import os, pathlib, sys, time\n"
        f"pathlib.Path({str(pid_file)!r}).write_text(str(os.getpid()))\n"
        "for _ in range(10000):\n"
        "    print('output', flush=True)\n"
        "time.sleep(60)\n"
    )

    def broken_sink(line: str) -> None:
        time.sleep(0.1)
        raise RuntimeError("sink failed")

    started = time.monotonic()
    with pytest.raises(RuntimeError, match="sink failed"):
        supervise(
            [PY, "-c", code],
            env=ENV,
            max_runtime_seconds=30,
            sink=broken_sink,
            heartbeat=lambda: None,
            timing=FAST,
        )
    assert time.monotonic() - started < 5
    assert _gone_soon(int(pid_file.read_text()))
    assert not any(thread.name == "mao-cloud-output-reader" for thread in threading.enumerate())


def test_successful_exit_is_not_timed_out_while_slow_output_drains() -> None:
    lines: list[str] = []

    def slow_first_line(line: str) -> None:
        if not lines:
            time.sleep(0.7)
        lines.append(line)

    result = supervise(
        [PY, "-c", "print('first'); print('second')"],
        env=ENV,
        max_runtime_seconds=0.5,
        sink=slow_first_line,
        heartbeat=lambda: None,
        timing=SupervisorTiming(flush_grace_seconds=2),
    )
    assert result.outcome is ChildOutcome.COMPLETED
    assert result.returncode == 0
    assert lines == ["first", "second"]


def test_slow_output_with_a_full_queue_still_times_out_and_stops_the_reader(monkeypatch) -> None:
    monkeypatch.setattr(trusted_supervise, "OUTPUT_QUEUE_LINES", 1)

    def slow_sink(line: str) -> None:
        time.sleep(0.03)

    started = time.monotonic()
    result = supervise(
        [PY, "-c", "import time; print('line\\n' * 10000, flush=True); time.sleep(60)"],
        env=ENV,
        max_runtime_seconds=0.2,
        sink=slow_sink,
        heartbeat=lambda: None,
        timing=FAST,
    )
    assert result.outcome is ChildOutcome.TIMED_OUT
    assert time.monotonic() - started < 5
    assert not any(thread.name == "mao-cloud-output-reader" for thread in threading.enumerate())


def test_raw_output_reports_truncation_for_newline_and_eof_lines() -> None:
    received: list[OutputLine] = []
    code = "import sys; sys.stdout.write('12345678\\nabcdefghTOO-LONG\\nfinal-line-too-long')"
    result, out = _supervise(
        [PY, "-c", code], line_limit=8, raw_sink=received.append
    )
    assert result.outcome is ChildOutcome.COMPLETED
    assert received == [
        OutputLine(text="12345678", truncated=False),
        OutputLine(text="abcdefgh", truncated=True),
        OutputLine(text="final-li", truncated=True),
    ]
    assert out.lines == []


def test_raw_output_preserves_json_and_pem_bytes_without_forwarding_to_the_log_sink(
    monkeypatch,
) -> None:
    monkeypatch.setattr(trusted_supervise, "READ_CHUNK_BYTES", 3)
    event = json.dumps(
        {
            "type": "message_end",
            "message": {
                "role": "assistant",
                "stopReason": "stop",
                "content": "token=placeholder -----BEGIN PRIVATE KEY-----\nSECRETBODY\n-----END PRIVATE KEY-----",
            },
        }
    )
    received: list[OutputLine] = []
    result, out = _supervise(
        [PY, "-c", f"print({event!r})"], raw_sink=received.append
    )
    assert result.outcome is ChildOutcome.COMPLETED
    assert received == [OutputLine(text=event, truncated=False)]
    assert out.lines == []
