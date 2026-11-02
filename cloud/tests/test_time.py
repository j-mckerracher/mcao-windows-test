"""Server-clock time rules: freshness, worker timestamp verdicts, deadlines, and liveness decisions."""

from datetime import timedelta
from typing import Any

from fakes import CLAIM_ID, DEFAULT_MAX_RUNTIME_SECONDS, EPOCH, admission_limits, run_record
from mao_cloud.protocol import (
    FailureCode,
    Freshness,
    RunState,
    TimestampVerdict,
    admission_deadline,
    classify_worker_timestamp,
    deadline_passed,
    failure,
    freshness,
    liveness_decision,
    upload_deadline,
)

STALE_AFTER = 120
SKEW = 30
STARTUP = 600
DEADLINE = EPOCH + timedelta(seconds=DEFAULT_MAX_RUNTIME_SECONDS)


def _at(seconds: float) -> Any:
    return EPOCH + timedelta(seconds=seconds)


def _liveness(record: Any, *, now: Any) -> Any:
    return liveness_decision(
        record,
        now=now,
        startup_timeout_seconds=STARTUP,
        stale_after_seconds=STALE_AFTER,
        skew_seconds=SKEW,
        queued_deadline_seconds=admission_limits().queued_deadline_seconds,
    )


def _live(state: RunState, *, last_seen: Any = EPOCH, **overrides: Any) -> Any:
    values: dict[str, Any] = {
        "state": state,
        "claim_id": CLAIM_ID,
        "version": 3,
        "last_seen_at": last_seen,
    }
    values.update(overrides)
    return run_record(**values)


# --- freshness: server receipt time only --------------------------------------------------------


def test_no_received_heartbeat_is_stale() -> None:
    result = freshness(last_seen_at=None, now=EPOCH, stale_after_seconds=STALE_AFTER, skew_seconds=SKEW)
    assert result is Freshness.STALE


def test_heartbeat_from_the_future_beyond_skew_is_a_clock_anomaly() -> None:
    result = freshness(
        last_seen_at=_at(SKEW + 1), now=EPOCH, stale_after_seconds=STALE_AFTER, skew_seconds=SKEW
    )
    assert result is Freshness.CLOCK_ANOMALY


def test_heartbeat_at_the_skew_limit_is_not_an_anomaly() -> None:
    result = freshness(
        last_seen_at=_at(SKEW), now=EPOCH, stale_after_seconds=STALE_AFTER, skew_seconds=SKEW
    )
    assert result is Freshness.FRESH


def test_heartbeat_exactly_stale_after_seconds_old_is_still_fresh() -> None:
    result = freshness(
        last_seen_at=EPOCH, now=_at(STALE_AFTER), stale_after_seconds=STALE_AFTER, skew_seconds=SKEW
    )
    assert result is Freshness.FRESH


def test_heartbeat_one_second_past_the_window_is_stale() -> None:
    result = freshness(
        last_seen_at=EPOCH,
        now=_at(STALE_AFTER + 1),
        stale_after_seconds=STALE_AFTER,
        skew_seconds=SKEW,
    )
    assert result is Freshness.STALE


# --- worker timestamps: informational verdicts ------------------------------------------------


def test_worker_timestamp_far_ahead_of_server_time_is_future() -> None:
    verdict = classify_worker_timestamp(_at(SKEW + 1), now=EPOCH, skew_seconds=SKEW)
    assert verdict is TimestampVerdict.FUTURE


def test_worker_timestamp_within_skew_is_accepted() -> None:
    verdict = classify_worker_timestamp(_at(SKEW), now=EPOCH, skew_seconds=SKEW)
    assert verdict is TimestampVerdict.ACCEPTED


def test_worker_timestamp_earlier_than_the_previous_one_is_backward() -> None:
    verdict = classify_worker_timestamp(
        _at(5), now=_at(10), skew_seconds=SKEW, previous=_at(9)
    )
    assert verdict is TimestampVerdict.BACKWARD


def test_worker_timestamp_equal_to_the_previous_one_is_accepted() -> None:
    verdict = classify_worker_timestamp(
        _at(9), now=_at(10), skew_seconds=SKEW, previous=_at(9)
    )
    assert verdict is TimestampVerdict.ACCEPTED


# --- deadlines ----------------------------------------------------------------------------------


def test_deadline_is_not_passed_one_second_early() -> None:
    assert deadline_passed(_at(DEFAULT_MAX_RUNTIME_SECONDS - 1), DEADLINE) is False


def test_deadline_is_passed_exactly_at_its_instant() -> None:
    assert deadline_passed(DEADLINE, DEADLINE) is True


def test_admission_deadline_is_admission_plus_the_runtime_budget() -> None:
    assert admission_deadline(admitted_at=EPOCH, max_runtime_seconds=900) == _at(900)


def test_upload_window_closes_after_ten_minutes_when_the_deadline_is_later() -> None:
    assert upload_deadline(admitted_at=EPOCH, deadline_at=DEADLINE) == _at(600)


def test_upload_window_is_capped_by_a_shorter_run_deadline() -> None:
    assert upload_deadline(admitted_at=EPOCH, deadline_at=_at(300)) == _at(300)


# --- liveness: each timeout boundary ----------------------------------------------------------


def test_awaiting_upload_stays_open_until_the_window_closes() -> None:
    record = run_record(state=RunState.AWAITING_UPLOAD, version=1, last_seen_at=None)
    assert _liveness(record, now=_at(599)) is None


def test_awaiting_upload_times_out_when_the_window_closes() -> None:
    record = run_record(state=RunState.AWAITING_UPLOAD, version=1, last_seen_at=None)
    decision = _liveness(record, now=_at(600))
    assert decision.to is RunState.TIMED_OUT
    assert decision.failure.code is FailureCode.UPLOAD_EXPIRED


def test_queued_run_has_until_thirty_minutes_before_timing_out() -> None:
    record = run_record(state=RunState.QUEUED, version=1, state_entered_at=EPOCH)
    assert _liveness(record, now=_at(1799)) is None


def test_queued_run_times_out_at_thirty_minutes() -> None:
    record = run_record(state=RunState.QUEUED, version=1, state_entered_at=EPOCH)
    decision = _liveness(record, now=_at(1800))
    assert decision.to is RunState.TIMED_OUT
    assert decision.failure.code is FailureCode.QUEUED_EXPIRED


def test_dispatching_within_the_startup_budget_is_left_alone() -> None:
    record = _live(RunState.DISPATCHING, last_seen=None, state_entered_at=EPOCH)
    assert _liveness(record, now=_at(STARTUP)) is None


def test_dispatching_past_the_startup_budget_is_reconciled() -> None:
    record = _live(RunState.DISPATCHING, last_seen=None, state_entered_at=EPOCH)
    decision = _liveness(record, now=_at(STARTUP + 1))
    assert decision.to is RunState.RECONCILING
    assert decision.failure is None


def test_starting_run_past_startup_fails_before_it_is_considered_stale() -> None:
    record = _live(RunState.STARTING, last_seen=EPOCH, state_entered_at=EPOCH)
    decision = _liveness(record, now=_at(STARTUP + 1))
    assert decision.to is RunState.FAILED
    assert decision.failure.code is FailureCode.STARTUP_TIMEOUT


def test_stale_starting_run_inside_the_startup_budget_is_reconciled() -> None:
    record = _live(RunState.STARTING, last_seen=EPOCH, state_entered_at=EPOCH)
    decision = _liveness(record, now=_at(STALE_AFTER + 1))
    assert decision.to is RunState.RECONCILING


def test_fresh_starting_run_is_left_alone() -> None:
    record = _live(RunState.STARTING, last_seen=EPOCH, state_entered_at=EPOCH)
    assert _liveness(record, now=_at(60)) is None


def test_running_run_is_reconciled_once_its_heartbeat_is_stale() -> None:
    record = _live(RunState.RUNNING, last_seen=EPOCH, state_entered_at=EPOCH)
    decision = _liveness(record, now=_at(STALE_AFTER + 1))
    assert decision.to is RunState.RECONCILING


def test_running_run_at_exactly_the_stale_window_is_not_yet_reconciled() -> None:
    record = _live(RunState.RUNNING, last_seen=EPOCH, state_entered_at=EPOCH)
    assert _liveness(record, now=_at(STALE_AFTER)) is None


def test_clock_anomaly_changes_nothing_for_a_running_run() -> None:
    record = _live(RunState.RUNNING, last_seen=_at(60 + SKEW + 1), state_entered_at=EPOCH)
    assert _liveness(record, now=_at(60)) is None


def test_reconciling_run_with_no_claim_ever_seen_fails_as_launch_uncertain() -> None:
    record = _live(RunState.RECONCILING, last_seen=None, state_entered_at=EPOCH)
    decision = _liveness(record, now=_at(STALE_AFTER))
    assert decision.to is RunState.FAILED
    assert decision.failure.code is FailureCode.LAUNCH_UNCERTAIN


def test_reconciling_run_that_was_once_alive_fails_as_heartbeat_lost() -> None:
    record = _live(RunState.RECONCILING, last_seen=EPOCH, state_entered_at=EPOCH)
    decision = _liveness(record, now=_at(STALE_AFTER + 1))
    assert decision.to is RunState.FAILED
    assert decision.failure.code is FailureCode.HEARTBEAT_LOST


def test_reconciling_run_is_given_the_full_stale_window_before_failing() -> None:
    record = _live(RunState.RECONCILING, last_seen=None, state_entered_at=EPOCH)
    assert _liveness(record, now=_at(STALE_AFTER - 1)) is None


def test_owner_cancelled_run_whose_worker_went_silent_waits_for_confirmed_stop() -> None:
    record = _live(
        RunState.CANCEL_REQUESTED,
        last_seen=None,
        state_entered_at=EPOCH,
        failure=failure(FailureCode.OWNER_CANCELLED),
    )
    assert _liveness(record, now=_at(STALE_AFTER + 1)) is None


def test_owner_cancelled_run_past_its_deadline_waits_for_confirmed_stop() -> None:
    record = _live(
        RunState.CANCEL_REQUESTED,
        last_seen=None,
        state_entered_at=EPOCH,
        failure=failure(FailureCode.OWNER_CANCELLED),
    )
    assert _liveness(record, now=DEADLINE + timedelta(seconds=1)) is None


def test_cancel_requested_run_with_a_live_worker_waits_for_it() -> None:
    record = _live(
        RunState.CANCEL_REQUESTED,
        last_seen=EPOCH,
        state_entered_at=EPOCH,
        failure=failure(FailureCode.OWNER_CANCELLED),
    )
    assert _liveness(record, now=_at(60)) is None


def test_deadline_cancel_past_the_deadline_times_out() -> None:
    record = _live(
        RunState.CANCEL_REQUESTED,
        last_seen=DEADLINE,
        state_entered_at=EPOCH,
        failure=failure(FailureCode.DEADLINE_EXCEEDED),
    )
    decision = _liveness(record, now=DEADLINE)
    assert decision.to is RunState.TIMED_OUT
    assert decision.failure.code is FailureCode.DEADLINE_EXCEEDED


def test_deadline_cancel_whose_worker_went_silent_fails_as_heartbeat_lost() -> None:
    record = _live(
        RunState.CANCEL_REQUESTED,
        last_seen=EPOCH,
        state_entered_at=EPOCH,
        failure=failure(FailureCode.DEADLINE_EXCEEDED),
    )
    decision = _liveness(record, now=_at(STALE_AFTER + 1))
    assert decision.to is RunState.FAILED
    assert decision.failure.code is FailureCode.HEARTBEAT_LOST


def test_running_run_past_its_deadline_is_asked_to_cancel() -> None:
    record = _live(RunState.RUNNING, last_seen=DEADLINE, state_entered_at=EPOCH)
    decision = _liveness(record, now=DEADLINE)
    assert decision.to is RunState.CANCEL_REQUESTED
    assert decision.failure.code is FailureCode.DEADLINE_EXCEEDED


def test_terminal_run_never_has_a_liveness_decision() -> None:
    record = _live(RunState.SUCCEEDED, last_seen=EPOCH, state_entered_at=EPOCH)
    assert _liveness(record, now=DEADLINE + timedelta(days=1)) is None
