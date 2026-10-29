"""Pure admission rules for DPES-809: worker and queue limits, the team daily cap, dispatch choice,
capacity reservation, and settlement. No broker, store, or wall clock: every instant is explicit.
"""

from datetime import UTC, date, datetime, timedelta
from uuid import uuid4

import pytest

from fakes import EPOCH, OWNER_A, OWNER_B, admission_limits, create_payload, run_record
from mao_cloud.admission import check_queue_room, daily_seconds_used, select_next_dispatch
from mao_cloud.config import AdmissionLimits
from mao_cloud.protocol import (
    CreateRunManifest,
    FailureCode,
    ProtocolError,
    RunRecord,
    RunState,
    begin_dispatch,
    failure,
    retry,
    settle_capacity,
    transition,
)

OWNER_C = "owner-c"
CLAIM_ID = "claim_" + "c" * 32
DAY = date(2026, 10, 21)  # the UTC day of EPOCH
CAP = 8 * 3600


def _run_id(n: int) -> str:
    return f"run_{n:032x}"


def _manifest(budget_seconds: int) -> CreateRunManifest:
    body = {
        key: value
        for key, value in create_payload(budget={"max_runtime_seconds": budget_seconds}).items()
        if key != "canonical_digest"
    }
    return CreateRunManifest.model_validate(body)


def _run(n: int, owner: str, state: RunState, *, budget: int = 600, **fields: object) -> RunRecord:
    record = run_record(run_id=_run_id(n), owner_id=owner, state=state, **fields)
    return record.model_copy(update={"manifest": _manifest(budget)})


def _holding(
    n: int,
    owner: str,
    state: RunState = RunState.RUNNING,
    *,
    dispatched_at: datetime = EPOCH,
    budget: int = 600,
    dispatch_order: int = 0,
) -> RunRecord:
    """A run that holds a worker slot and its full reservation."""
    return _run(
        n,
        owner,
        state,
        budget=budget,
        dispatched_at=dispatched_at,
        dispatch_order=dispatch_order,
        reserved_seconds=budget,
        claim_id=CLAIM_ID,
    )


DEFAULT_LIMITS = admission_limits()


def _selected(
    records: list[RunRecord],
    *,
    now: datetime = EPOCH + timedelta(seconds=10),
    limits: AdmissionLimits = DEFAULT_LIMITS,
) -> str | None:
    chosen = select_next_dispatch(records, now=now, limits=limits)
    return None if chosen is None else chosen.run_id


# -- team and owner worker limits ----------------------------------------------


def test_team_limit_holds_the_fifth_dispatch_back() -> None:
    holders = [
        _holding(1, OWNER_A),
        _holding(2, OWNER_A),
        _holding(3, OWNER_A),
        _holding(4, OWNER_B),
    ]
    assert _selected([*holders, _run(9, OWNER_B, RunState.QUEUED)]) is None


def test_three_holders_leave_one_slot_for_the_queued_run() -> None:
    holders = [_holding(1, OWNER_A), _holding(2, OWNER_A), _holding(3, OWNER_B)]
    assert _selected([*holders, _run(9, OWNER_B, RunState.QUEUED)]) == _run_id(9)


def test_a_cancel_requested_run_still_holds_its_slot() -> None:
    holders = [
        _holding(1, OWNER_A),
        _holding(2, OWNER_A),
        _holding(3, OWNER_B),
        _holding(4, OWNER_B, RunState.CANCEL_REQUESTED),
    ]
    assert _selected([*holders, _run(9, OWNER_B, RunState.QUEUED)]) is None


def test_owner_limit_gates_that_owner_before_the_team_limit() -> None:
    limits = admission_limits(owner_worker_slots=2)
    holders = [_holding(1, OWNER_A), _holding(2, OWNER_A)]
    queued = [_run(9, OWNER_A, RunState.QUEUED), _run(8, OWNER_B, RunState.QUEUED)]
    assert _selected([*holders, *queued], limits=limits) == _run_id(8)


# -- queue depth ---------------------------------------------------------------


def test_team_queue_refuses_the_twenty_first_queued_run() -> None:
    queued = [_run(i, OWNER_A if i < 10 else OWNER_B, RunState.QUEUED) for i in range(20)]
    with pytest.raises(ProtocolError) as refused:
        check_queue_room(queued, owner_id=OWNER_C, limits=DEFAULT_LIMITS)
    assert refused.value.code == "queue_full"


def test_owner_queue_refuses_the_eleventh_run_for_that_owner_only() -> None:
    queued = [_run(i, OWNER_A, RunState.QUEUED) for i in range(10)]
    with pytest.raises(ProtocolError) as refused:
        check_queue_room(queued, owner_id=OWNER_A, limits=DEFAULT_LIMITS)
    assert refused.value.code == "queue_full"
    check_queue_room(queued, owner_id=OWNER_B, limits=DEFAULT_LIMITS)


def test_only_queued_runs_count_toward_queue_depth() -> None:
    running = [_holding(i, OWNER_A) for i in range(10)]
    check_queue_room(running, owner_id=OWNER_A, limits=DEFAULT_LIMITS)


# -- choice among queued runs --------------------------------------------------


def test_queued_runs_dispatch_first_in_first_out_within_an_owner() -> None:
    later = _run(2, OWNER_A, RunState.QUEUED, state_entered_at=EPOCH + timedelta(seconds=5))
    earlier = _run(1, OWNER_A, RunState.QUEUED, state_entered_at=EPOCH + timedelta(seconds=1))
    assert _selected([later, earlier]) == _run_id(1)


def test_owners_take_turns_starting_after_the_owner_of_the_latest_dispatch() -> None:
    latest = _holding(1, OWNER_A, dispatched_at=EPOCH + timedelta(seconds=5))
    a_queued = _run(9, OWNER_A, RunState.QUEUED, state_entered_at=EPOCH)
    b_queued = _run(8, OWNER_B, RunState.QUEUED, state_entered_at=EPOCH + timedelta(seconds=1))
    assert _selected([latest, a_queued, b_queued]) == _run_id(8)
    # The answer does not depend on the order the records arrive in.
    assert _selected([b_queued, a_queued, latest]) == _run_id(8)


def test_round_robin_wraps_back_to_the_owner_that_waited_longest() -> None:
    a_earlier = _run(1, OWNER_A, RunState.SUCCEEDED, dispatched_at=EPOCH + timedelta(seconds=2))
    b_latest = _holding(2, OWNER_B, dispatched_at=EPOCH + timedelta(seconds=5))
    a_queued = _run(9, OWNER_A, RunState.QUEUED, state_entered_at=EPOCH)
    b_queued = _run(8, OWNER_B, RunState.QUEUED, state_entered_at=EPOCH)
    assert _selected([b_latest, b_queued, a_earlier, a_queued]) == _run_id(9)


# -- daily reservation cap and admission-day attribution -----------------------


def test_daily_cap_refuses_a_run_that_would_pass_eight_worker_hours() -> None:
    settled = _run(1, OWNER_A, RunState.SUCCEEDED, consumed_seconds=CAP - 600)
    assert _selected([settled, _run(3, OWNER_A, RunState.QUEUED, budget=2400)]) is None


def test_daily_cap_admits_a_run_that_ends_exactly_on_eight_worker_hours() -> None:
    settled = _run(1, OWNER_A, RunState.SUCCEEDED, consumed_seconds=CAP - 600)
    assert _selected([settled, _run(2, OWNER_A, RunState.QUEUED, budget=600)]) == _run_id(2)


def test_a_live_reservation_counts_against_the_daily_cap_like_settled_usage() -> None:
    reserving = _run(
        1,
        OWNER_B,
        RunState.RUNNING,
        dispatched_at=EPOCH,
        reserved_seconds=CAP - 600,
        claim_id=CLAIM_ID,
    )
    assert _selected([reserving, _run(3, OWNER_A, RunState.QUEUED, budget=2400)]) is None


def test_the_daily_cap_is_shared_by_the_whole_team() -> None:
    settled_by_b = _run(1, OWNER_B, RunState.SUCCEEDED, consumed_seconds=CAP - 600)
    assert _selected([settled_by_b, _run(2, OWNER_A, RunState.QUEUED, budget=2400)]) is None
    assert _selected([settled_by_b, _run(3, OWNER_A, RunState.QUEUED, budget=600)]) == _run_id(3)


def test_the_daily_cap_resets_on_the_next_utc_day() -> None:
    settled = _run(1, OWNER_A, RunState.SUCCEEDED, consumed_seconds=CAP - 600)
    next_day = _run(
        3,
        OWNER_A,
        RunState.QUEUED,
        budget=2400,
        admitted_at=EPOCH + timedelta(hours=13),
    )
    assert _selected([settled, next_day]) == _run_id(3)


def test_a_run_crossing_midnight_stays_charged_to_its_admission_day() -> None:
    admitted = datetime(2026, 10, 21, 23, 40, tzinfo=UTC)
    dispatched = datetime(2026, 10, 22, 0, 5, tzinfo=UTC)
    settled_at = datetime(2026, 10, 22, 0, 20, tzinfo=UTC)
    dispatching = _run(
        1,
        OWNER_A,
        RunState.DISPATCHING,
        budget=1800,
        admitted_at=admitted,
        dispatched_at=dispatched,
        reserved_seconds=1800,
        claim_id=CLAIM_ID,
    )
    settled = transition(
        dispatching,
        to=RunState.FAILED,
        expected_version=dispatching.version,
        expected_attempt=dispatching.attempt,
        now=settled_at,
        failure=failure(FailureCode.WORKER_FAILED),
    )
    settled = settle_capacity(settled, now=settled_at)
    assert settled.consumed_seconds == 900
    assert daily_seconds_used([settled], day=DAY) == 900
    assert daily_seconds_used([settled], day=date(2026, 10, 22)) == 0


# -- reservation and settlement ------------------------------------------------


def test_begin_dispatch_reserves_the_full_budget_and_stamps_the_dispatch() -> None:
    queued = _run(1, OWNER_A, RunState.QUEUED, budget=900)
    now = EPOCH + timedelta(seconds=5)
    dispatching = begin_dispatch(
        queued,
        claim_id=CLAIM_ID,
        expected_version=queued.version,
        expected_attempt=queued.attempt,
        now=now,
        dispatch_order=37,
    )
    assert dispatching.dispatch_order == 37
    assert dispatching.state is RunState.DISPATCHING
    assert dispatching.reserved_seconds == 900
    assert dispatching.dispatched_at == now
    assert dispatching.consumed_seconds == 0


def test_termination_settles_the_reservation_to_actual_elapsed_seconds() -> None:
    dispatching = _holding(1, OWNER_A, RunState.DISPATCHING, budget=600)
    settled = transition(
        dispatching,
        to=RunState.FAILED,
        expected_version=dispatching.version,
        expected_attempt=dispatching.attempt,
        now=EPOCH + timedelta(seconds=90),
        failure=failure(FailureCode.WORKER_FAILED),
    )
    assert (settled.consumed_seconds, settled.reserved_seconds) == (0, 600)
    settled = settle_capacity(settled, now=EPOCH + timedelta(seconds=90))
    assert (settled.consumed_seconds, settled.reserved_seconds) == (90, 0)


def test_settlement_never_charges_more_than_the_reservation() -> None:
    dispatching = _holding(1, OWNER_A, RunState.DISPATCHING, budget=600)
    settled = transition(
        dispatching,
        to=RunState.FAILED,
        expected_version=dispatching.version,
        expected_attempt=dispatching.attempt,
        now=EPOCH + timedelta(seconds=1000),
        failure=failure(FailureCode.WORKER_FAILED),
    )
    settled = settle_capacity(settled, now=EPOCH + timedelta(seconds=1000))
    assert (settled.consumed_seconds, settled.reserved_seconds) == (600, 0)


def test_a_non_terminal_transition_keeps_the_reservation() -> None:
    dispatching = _holding(1, OWNER_A, RunState.DISPATCHING, budget=600)
    reconciling = transition(
        dispatching,
        to=RunState.RECONCILING,
        expected_version=dispatching.version,
        expected_attempt=dispatching.attempt,
        now=EPOCH + timedelta(seconds=601),
    )
    assert (reconciling.consumed_seconds, reconciling.reserved_seconds) == (0, 600)


def test_retry_carries_settled_usage_forward_and_the_next_dispatch_reserves_again() -> None:
    failed = _run(
        1,
        OWNER_A,
        RunState.FAILED,
        failure=failure(FailureCode.LAUNCH_UNCERTAIN),
        consumed_seconds=300,
        dispatch_order=41,
    )
    requeued = retry(
        failed,
        request_uuid=uuid4(),
        expected_version=failed.version,
        expected_attempt=failed.attempt,
        now=EPOCH + timedelta(seconds=600),
    )
    assert requeued.state is RunState.QUEUED
    assert requeued.dispatch_order == 41
    assert (requeued.consumed_seconds, requeued.reserved_seconds) == (300, 0)
    dispatching = begin_dispatch(
        requeued,
        claim_id=CLAIM_ID,
        expected_version=requeued.version,
        expected_attempt=requeued.attempt,
        now=EPOCH + timedelta(seconds=601),
    )
    assert dispatching.reserved_seconds == 600
    assert dispatching.consumed_seconds == 300


@pytest.mark.parametrize("expired_by", ["overall_deadline", "queue_deadline"])
def test_expired_queue_head_does_not_block_its_owners_next_live_run(expired_by: str) -> None:
    now = EPOCH + timedelta(seconds=1800)
    expired = _run(
        1,
        OWNER_A,
        RunState.QUEUED,
        state_entered_at=EPOCH if expired_by == "queue_deadline" else now - timedelta(seconds=2),
        deadline_at=now if expired_by == "overall_deadline" else now + timedelta(hours=1),
    )
    live = _run(2, OWNER_A, RunState.QUEUED, state_entered_at=now - timedelta(seconds=1))
    assert _selected([expired, live], now=now) == live.run_id
    assert _selected([live, expired], now=now) == live.run_id


def test_persisted_dispatch_order_rotates_owners_with_equal_dispatch_timestamps() -> None:
    a = _holding(1, OWNER_A, dispatched_at=EPOCH, dispatch_order=10)
    b = _holding(2, OWNER_B, dispatched_at=EPOCH, dispatch_order=11)
    a_queued = _run(9, OWNER_A, RunState.QUEUED, state_entered_at=EPOCH + timedelta(seconds=2))
    b_queued = _run(8, OWNER_B, RunState.QUEUED, state_entered_at=EPOCH + timedelta(seconds=1))
    assert _selected([a, b, b_queued, a_queued]) == a_queued.run_id
    assert _selected([a_queued, b_queued, b, a]) == a_queued.run_id
    a_next = _holding(3, OWNER_A, dispatched_at=EPOCH, dispatch_order=12)
    assert _selected([a, b, a_next, a_queued, b_queued]) == b_queued.run_id


def test_terminal_attempt_keeps_its_worker_slot_until_capacity_is_settled() -> None:
    holders = [_holding(1, OWNER_A), _holding(2, OWNER_A), _holding(3, OWNER_B)]
    failed = _holding(4, OWNER_B, RunState.FAILED)
    queued = _run(9, OWNER_B, RunState.QUEUED)
    assert _selected([*holders, failed, queued]) is None
    settled = settle_capacity(failed, now=EPOCH + timedelta(seconds=90))
    assert settled.version == failed.version + 1
    assert _selected([*holders, settled, queued]) == queued.run_id
    assert settle_capacity(settled, now=EPOCH + timedelta(seconds=100)) is settled


def test_capacity_cannot_be_settled_for_a_nonterminal_attempt() -> None:
    running = _holding(1, OWNER_A)
    with pytest.raises(ProtocolError) as refused:
        settle_capacity(running, now=EPOCH + timedelta(seconds=90))
    assert refused.value.code == "illegal_transition"


def test_retry_refuses_a_failed_attempt_whose_execution_still_holds_capacity() -> None:
    failed = _holding(1, OWNER_A, RunState.FAILED).model_copy(
        update={"failure": failure(FailureCode.LAUNCH_UNCERTAIN)}
    )
    with pytest.raises(ProtocolError) as refused:
        retry(
            failed,
            request_uuid=uuid4(),
            expected_version=failed.version,
            expected_attempt=failed.attempt,
            now=EPOCH + timedelta(seconds=90),
        )
    assert refused.value.code == "retry_not_allowed"
