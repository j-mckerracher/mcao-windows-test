"""Trusted supervision: run the worker's process group, heartbeat it, checkpoint it, classify it.

DPES-811 AC5. The child leads its own process group, so a deadline or a stray grandchild stops the
whole tree. Output is read by one thread, split into bounded lines, redacted, then passed to the
sink from the supervising thread. A child counts as completed only on a clean zero exit. A signal,
including the kernel OOM killer's SIGKILL, is reported as killed, never as success.
"""

import os
import queue
import select
import signal
import subprocess
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass
from enum import StrEnum
from typing import IO

from mao_cloud.protocol import MAX_LOG_LINE_CHARS, redact

POLL_SECONDS = 0.02
READ_CHUNK_BYTES = 64 * 1024
OUTPUT_QUEUE_LINES = 64
# Queue item for the end of the child's output. OutputLine cannot collide with None.
_END_OF_OUTPUT = None


class ChildOutcome(StrEnum):
    COMPLETED = "completed"
    FAILED = "failed"
    KILLED = "killed"
    TIMED_OUT = "timed_out"


@dataclass(frozen=True)
class ChildResult:
    outcome: ChildOutcome
    returncode: int


@dataclass(frozen=True)
class OutputLine:
    """A bounded output prefix and whether any bytes from that physical line were dropped."""

    text: str
    truncated: bool = False


@dataclass(frozen=True)
class SupervisorTiming:
    """Production values: heartbeat every 15 s, checkpoint at most every 5 min, bounded flush."""

    heartbeat_seconds: float = 15.0
    checkpoint_min_interval_seconds: float = 300.0
    flush_grace_seconds: float = 30.0


def checkpoint_due(*, last: float | None, now: float, changed: bool, min_interval: float) -> bool:
    """A checkpoint is due only for a change, and never sooner than min_interval after the last."""
    if not changed:
        return False
    return last is None or now - last >= min_interval


class _LineSplitter:
    """Split raw output into lines no longer than limit bytes, holding no more than limit bytes.

    An over-long line keeps its first limit bytes and drops the rest up to its newline. A line that
    never ends therefore cannot grow the buffer, and the lines after it still arrive. Decoded text is
    never longer than limit characters, since each character takes at least one byte.
    """

    def __init__(self, limit: int = MAX_LOG_LINE_CHARS) -> None:
        self._limit = limit
        self._head = bytearray()
        self._dropping = False
        self._open = False

    def feed(self, data: bytes) -> list[OutputLine]:
        lines: list[OutputLine] = []
        while True:
            newline = data.find(b"\n")
            if newline < 0:
                self._take(data)
                return lines
            self._take(data[:newline])
            lines.append(self._finish())
            data = data[newline + 1 :]

    def flush(self) -> list[OutputLine]:
        return [self._finish()] if self._open else []

    def _take(self, chunk: bytes) -> None:
        if chunk:
            self._open = True
        if self._dropping:
            return
        room = self._limit - len(self._head)
        self._head += chunk[:room]
        if len(chunk) > room:
            self._dropping = True

    def _finish(self) -> OutputLine:
        line = OutputLine(
            text=bytes(self._head).decode("utf-8", errors="replace"),
            truncated=self._dropping,
        )
        self._head.clear()
        self._dropping = False
        self._open = False
        return line


class _PemRedactor:
    """Suppress PEM blocks before line truncation, retaining only a partial marker.

    All PEM blocks are suppressed conservatively so private-key label lengths and chunk
    boundaries cannot defeat redaction. The block body is discarded, never accumulated.
    """

    _BEGIN = b"-----BEGIN "
    _END = b"-----END "
    _CLOSE = b"-----"

    def __init__(self) -> None:
        self._pending = b""
        self._inside = False
        self._ending = False

    def feed(self, data: bytes) -> bytes:
        self._pending += data
        safe = bytearray()
        while True:
            marker = self._CLOSE if self._ending else self._END if self._inside else self._BEGIN
            match = self._pending.find(marker)
            if match < 0:
                count = max(0, len(self._pending) - len(marker) + 1)
                if not self._inside:
                    safe.extend(self._pending[:count])
                self._pending = self._pending[count:]
                return bytes(safe)
            if not self._inside:
                safe.extend(self._pending[:match])
                safe.extend(b"[REDACTED]")
                self._inside = True
            elif self._ending:
                self._inside = False
                self._ending = False
            else:
                self._ending = True
            self._pending = self._pending[match + len(marker) :]

    def flush(self) -> bytes:
        safe = b"" if self._inside else self._pending
        self._pending = b""
        return safe


def _enqueue(
    out: "queue.Queue[OutputLine | None]", item: OutputLine | None, stopped: threading.Event
) -> bool:
    while not stopped.is_set():
        try:
            out.put(item, timeout=POLL_SECONDS)
            return True
        except queue.Full:
            continue
    return False


def _pump(
    stream: IO[bytes],
    out: "queue.Queue[OutputLine | None]",
    stopped: threading.Event,
    limit: int,
    raw_output: bool,
) -> None:
    """Bound output backlog and stop even when the consumer or a pipe writer disappears."""
    splitter = _LineSplitter(limit)
    pem = None if raw_output else _PemRedactor()
    try:
        while not stopped.is_set():
            readable, _, _ = select.select([stream], [], [], POLL_SECONDS)
            if not readable:
                continue
            chunk = os.read(stream.fileno(), READ_CHUNK_BYTES)
            if not chunk:
                break
            output = chunk if pem is None else pem.feed(chunk)
            for line in splitter.feed(output):
                if not _enqueue(out, line, stopped):
                    return
        if not stopped.is_set():
            pending = b"" if pem is None else pem.flush()
            for line in splitter.feed(pending) + splitter.flush():
                if not _enqueue(out, line, stopped):
                    return
    finally:
        _enqueue(out, _END_OF_OUTPUT, stopped)


def _signal_group(pgid: int, sig: int) -> None:
    try:
        os.killpg(pgid, sig)
    except ProcessLookupError:
        pass


def _classify(*, timed_out: bool, returncode: int) -> ChildOutcome:
    if timed_out:
        return ChildOutcome.TIMED_OUT
    if returncode == 0:
        return ChildOutcome.COMPLETED
    if returncode < 0:
        return ChildOutcome.KILLED
    return ChildOutcome.FAILED


def supervise(
    argv: list[str],
    *,
    env: dict[str, str],
    max_runtime_seconds: float,
    sink: Callable[[str], None],
    heartbeat: Callable[[], None],
    timing: SupervisorTiming,
    fingerprint: Callable[[], str] | None = None,
    checkpoint: Callable[[], None] | None = None,
    clock: Callable[[], float] = time.monotonic,
    line_limit: int = MAX_LOG_LINE_CHARS,
    raw_sink: Callable[[OutputLine], None] | None = None,
) -> ChildResult:
    """Run argv in its own process group until it exits or the runtime ends. Returns its outcome.

    On the deadline the group gets SIGTERM for a bounded flush, then SIGKILL. A checkpoint is taken
    only when fingerprint shows a change since the last checkpoint, and no sooner than the minimum
    interval. The baseline fingerprint is taken at start, so an unchanged workspace never checkpoints.
    line_limit caps each output line in bytes. raw_sink opts a trusted parser into original
    bounded lines and truncation metadata instead of redacted sink delivery. It bypasses both PEM
    filtering and text redaction; that parser must whitelist any records it forwards to logs.
    """
    proc = subprocess.Popen(
        argv,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    if proc.stdout is None:
        raise RuntimeError("the child's output pipe was not created")
    lines: queue.Queue[OutputLine | None] = queue.Queue(maxsize=OUTPUT_QUEUE_LINES)
    stopped = threading.Event()
    reader = threading.Thread(
        target=_pump,
        args=(proc.stdout, lines, stopped, line_limit, raw_sink is not None),
        name="mao-cloud-output-reader",
        daemon=True,
    )
    reader.start()
    try:
        return _watch(
            proc,
            lines,
            max_runtime_seconds=max_runtime_seconds,
            sink=sink,
            raw_sink=raw_sink,
            heartbeat=heartbeat,
            timing=timing,
            fingerprint=fingerprint,
            checkpoint=checkpoint,
            clock=clock,
        )
    finally:
        stopped.set()
        _signal_group(proc.pid, signal.SIGKILL)
        if proc.poll() is None:
            proc.wait()
        reader.join()
        proc.stdout.close()


def _watch(
    proc: subprocess.Popen[bytes],
    lines: "queue.Queue[OutputLine | None]",
    *,
    max_runtime_seconds: float,
    sink: Callable[[str], None],
    raw_sink: Callable[[OutputLine], None] | None,
    heartbeat: Callable[[], None],
    timing: SupervisorTiming,
    fingerprint: Callable[[], str] | None,
    checkpoint: Callable[[], None] | None,
    clock: Callable[[], float],
) -> ChildResult:
    start = clock()
    deadline = start + max_runtime_seconds
    next_beat = start + timing.heartbeat_seconds
    checkpointed = fingerprint() if fingerprint is not None else None
    last_checkpoint: float | None = None
    timed_out = False
    kill_at: float | None = None
    exit_grace_end: float | None = None
    eof = False
    returncode: int | None = None

    while True:
        now = clock()
        if returncode is None:
            returncode = proc.poll()
            if returncode is not None:
                # The leader is gone; stop any grandchildren it left in the group.
                _signal_group(proc.pid, signal.SIGKILL)
                exit_grace_end = now + timing.flush_grace_seconds
        if returncode is None and not timed_out and now >= deadline:
            timed_out = True
            _signal_group(proc.pid, signal.SIGTERM)
            kill_at = now + timing.flush_grace_seconds
        if kill_at is not None and now >= kill_at:
            _signal_group(proc.pid, signal.SIGKILL)
            kill_at = None
        if now >= next_beat:
            heartbeat()
            next_beat = now + timing.heartbeat_seconds
            if fingerprint is not None and checkpoint is not None:
                current = fingerprint()
                if checkpoint_due(
                    last=last_checkpoint,
                    now=now,
                    changed=current != checkpointed,
                    min_interval=timing.checkpoint_min_interval_seconds,
                ):
                    checkpoint()
                    checkpointed = current
                    last_checkpoint = now
        if returncode is not None and exit_grace_end is not None:
            if eof or now >= exit_grace_end:
                break
        try:
            item = lines.get(timeout=POLL_SECONDS)
        except queue.Empty:
            continue
        if item is _END_OF_OUTPUT:
            eof = True
        elif raw_sink is not None:
            raw_sink(item)
        else:
            sink(redact(item.text).text)

    if returncode is None:
        raise RuntimeError("the child's exit status was not observed")
    outcome = _classify(timed_out=timed_out, returncode=returncode)
    return ChildResult(outcome=outcome, returncode=returncode)
