"""Admission rules for DPES-809: queue depth, worker slots, the team daily reservation cap, and
the choice of the next queued run.

Pure functions over run records, an explicit current instant, and the admission limits from the
policy document. No store, broker, or wall clock: the answer never depends on input order or on
when the function happens to run.
"""

from collections.abc import Sequence
from datetime import UTC, date, datetime

from mao_cloud.config import AdmissionLimits
from mao_cloud.protocol import (
    ACTIVE_STATES,
    ErrorCode,
    ProtocolError,
    RunRecord,
    RunState,
    deadline_passed,
)

# An active attempt or an unsettled reservation holds its worker slot, including after
# a terminal lifecycle update while execution termination is still uncertain.
_HOLDING = ACTIVE_STATES | {RunState.CANCEL_REQUESTED}

_EARLIEST = datetime.min.replace(tzinfo=UTC)


def holds_capacity(record: RunRecord) -> bool:
    return record.state in _HOLDING or record.reserved_seconds > 0


def attribution_day(record: RunRecord) -> date:
    """The UTC day a run is charged to: its admission day, even when it crosses midnight."""
    return record.admitted_at.astimezone(UTC).date()


def daily_seconds_used(records: Sequence[RunRecord], *, day: date) -> int:
    """Team-wide worker seconds attributed to day: settled usage plus live reservations."""
    return sum(
        record.consumed_seconds + record.reserved_seconds
        for record in records
        if attribution_day(record) == day
    )


def check_queue_room(
    records: Sequence[RunRecord], *, owner_id: str, limits: AdmissionLimits
) -> None:
    """Refuse a new queued run when the team or this owner already has a full queue."""
    queued = [record for record in records if record.state is RunState.QUEUED]
    if len(queued) >= limits.team_queue_depth:
        raise ProtocolError(
            ErrorCode.QUEUE_FULL,
            "the team queue is full; try again after queued runs start",
            scope="team",
            limit=limits.team_queue_depth,
        )
    owner_queued = sum(1 for record in queued if record.owner_id == owner_id)
    if owner_queued >= limits.owner_queue_depth:
        raise ProtocolError(
            ErrorCode.QUEUE_FULL,
            "this owner's queue is full; try again after queued runs start",
            scope="owner",
            limit=limits.owner_queue_depth,
        )


def select_next_dispatch(
    records: Sequence[RunRecord], *, now: datetime, limits: AdmissionLimits
) -> RunRecord | None:
    """The one queued run that may dispatch now, or None.

    Each owner's next run is its oldest queued run (FIFO). An owner is eligible when it has a
    free owner slot and that run fits the team's daily cap for its admission day. Among eligible
    owners, the one whose most recent dispatch order is oldest goes next. Legacy records
    without a persisted order use their dispatch timestamp. Owners with no dispatch yet go
    first. Expired queue entries cannot block another run. Input order never affects the answer.
    """
    holders = [record for record in records if holds_capacity(record)]
    if len(holders) >= limits.team_worker_slots:
        return None

    heads = _owner_heads(records, now=now, queued_deadline_seconds=limits.queued_deadline_seconds)
    last_dispatch = _last_dispatch_by_owner(records)
    eligible: list[tuple[tuple[bool, int, int, datetime, datetime, str], RunRecord]] = []
    for owner_id, head in heads.items():
        if sum(1 for holder in holders if holder.owner_id == owner_id) >= limits.owner_worker_slots:
            continue
        used = daily_seconds_used(records, day=attribution_day(head))
        if used + head.manifest.budget.max_runtime_seconds > limits.team_daily_worker_seconds:
            continue
        last = last_dispatch.get(owner_id)
        turn = (last is not None, *(last or (0, 0, _EARLIEST)), head.state_entered_at, head.run_id)
        eligible.append((turn, head))
    if not eligible:
        return None
    return min(eligible, key=lambda item: item[0])[1]


def _owner_heads(
    records: Sequence[RunRecord], *, now: datetime, queued_deadline_seconds: int
) -> dict[str, RunRecord]:
    """Each owner's oldest unexpired queued run, by (state_entered_at, run_id)."""
    heads: dict[str, RunRecord] = {}
    for record in records:
        if record.state is not RunState.QUEUED:
            continue
        queued_expired = (now - record.state_entered_at).total_seconds() >= queued_deadline_seconds
        if deadline_passed(now, record.deadline_at) or queued_expired:
            continue
        current = heads.get(record.owner_id)
        if current is None or (record.state_entered_at, record.run_id) < (
            current.state_entered_at,
            current.run_id,
        ):
            heads[record.owner_id] = record
    return heads


def _last_dispatch_by_owner(
    records: Sequence[RunRecord],
) -> dict[str, tuple[int, int, datetime]]:
    last: dict[str, tuple[int, int, datetime]] = {}
    for record in records:
        if record.dispatch_order == 0 and record.dispatched_at is None:
            continue
        dispatch = (
            int(record.dispatch_order > 0),
            record.dispatch_order,
            (record.dispatched_at or _EARLIEST) if record.dispatch_order == 0 else _EARLIEST,
        )
        if record.owner_id not in last or dispatch > last[record.owner_id]:
            last[record.owner_id] = dispatch
    return last
