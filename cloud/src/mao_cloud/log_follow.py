"""Follow a run's logs one bounded page at a time, with replay-safe cursors and capped backoff.

The engine knows nothing about HTTP, storage, or the clock. It asks a fetch port for one page,
yields what the page proves, and waits through an injected sleep when there is nothing new.
Entries come only from LogPage, so the redaction applied when each LogEntry was built is what a
follower sees. A page replayed after a lost response is deduplicated by seq. A gap in trimmed
history is reported once, for exactly the range the follower never saw, instead of being skipped
silently. Following ends when the run is terminal and every durable entry has been yielded.
"""

from __future__ import annotations

from collections.abc import Callable, Iterator
from dataclasses import dataclass

from mao_cloud.protocol import MAX_LOG_LIMIT, TERMINAL_STATES, LogEntry, LogPage, RunState


class TransientFetchError(Exception):
    """A page fetch failed in a way a later attempt may fix, such as a dropped connection or 5xx."""


@dataclass(frozen=True)
class Backoff:
    """Waits grow geometrically from initial_seconds and stop at max_seconds."""

    initial_seconds: float = 1.0
    factor: float = 2.0
    max_seconds: float = 30.0

    def __post_init__(self) -> None:
        if self.initial_seconds <= 0 or self.factor < 1 or self.max_seconds < self.initial_seconds:
            raise ValueError(
                "backoff needs a positive start, a factor of at least 1, "
                "and a cap at or above the start"
            )

    def after(self, previous: float | None) -> float:
        if previous is None:
            return self.initial_seconds
        return min(previous * self.factor, self.max_seconds)


@dataclass(frozen=True)
class EntryEvent:
    entry: LogEntry


@dataclass(frozen=True)
class GapEvent:
    """Entries from_seq through to_seq were trimmed before this follower could read them."""

    from_seq: int
    to_seq: int
    retained_from: int
    watermark: int


@dataclass(frozen=True)
class EndEvent:
    state: RunState
    watermark: int


FollowEvent = EntryEvent | GapEvent | EndEvent
FetchPage = Callable[[str | None, int], LogPage]


def follow_logs(
    fetch: FetchPage,
    *,
    sleep: Callable[[float], None],
    limit: int = MAX_LOG_LIMIT,
    backoff: Backoff = Backoff(),
    max_failures: int = 5,
    cursor: str | None = None,
) -> Iterator[FollowEvent]:
    """Yield entries, gaps, and a final end event, fetching at most `limit` entries per page.

    Argument errors raise here, before any fetch. `cursor` resumes a previous follow; None starts
    at the first entry of the current attempt.
    """
    if not 1 <= limit <= MAX_LOG_LIMIT:
        raise ValueError(f"limit must be between 1 and {MAX_LOG_LIMIT}")
    if max_failures < 0:
        raise ValueError("max_failures must not be negative")
    return _follow(fetch, sleep, limit, backoff, max_failures, cursor)


def _follow(
    fetch: FetchPage,
    sleep: Callable[[float], None],
    limit: int,
    backoff: Backoff,
    max_failures: int,
    cursor: str | None,
) -> Iterator[FollowEvent]:
    last_seq = 0
    wait: float | None = None
    failures = 0
    while True:
        try:
            page = fetch(cursor, limit)
        except TransientFetchError:
            failures += 1
            if failures > max_failures:
                raise
            wait = backoff.after(wait)
            sleep(wait)
            continue
        failures = 0

        if page.gap is not None:
            unseen_from = max(page.gap.from_seq, last_seq + 1)
            if unseen_from <= page.gap.to_seq:
                last_seq = page.gap.to_seq
                yield GapEvent(
                    from_seq=unseen_from,
                    to_seq=page.gap.to_seq,
                    retained_from=page.retained_from,
                    watermark=page.watermark,
                )

        for entry in page.entries:
            if entry.seq > last_seq:
                yield EntryEvent(entry)
                last_seq = entry.seq

        cursor = page.next_cursor
        if page.entries:
            wait = None
            continue
        # An empty page proves this cursor or the retained floor is beyond the watermark.
        if page.snapshot.state in TERMINAL_STATES:
            yield EndEvent(state=page.snapshot.state, watermark=page.watermark)
            return
        wait = backoff.after(wait)
        sleep(wait)
