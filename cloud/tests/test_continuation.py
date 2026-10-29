"""Continuation ledger and retry policy: literal outcomes for concrete inputs."""

from datetime import UTC, datetime, timedelta
from typing import Any

import pytest

from fakes import run_record
from mao_cloud.continuation import (
    AttemptCharge,
    CheckpointView,
    ContinuationDecision,
    ExecutionView,
    Interruption,
    KillSwitchClass,
    LogicalLedger,
    Reason,
    Trigger,
    Verdict,
    evaluate_continuation,
)
from mao_cloud.protocol import FailureCode, RunRecord, RunState, failure

NOW = datetime(2026, 10, 22, 12, 0, tzinfo=UTC)
DEADLINE = NOW + timedelta(seconds=3600)
MIDNIGHT_DEADLINE = datetime(2026, 10, 22, 1, 0, tzinfo=UTC)


def test_reserve_then_settle_charges_used_seconds() -> None:
    ledger = LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=3600)
    ledger = ledger.settle(attempt=1, used_seconds=600)

    assert ledger.charged_seconds == 600
    assert ledger.allowance_seconds == 6600
    assert ledger.charges == (
        AttemptCharge(attempt=1, reserved_seconds=3600, settled_seconds=600),
    )


def test_unsettled_reservation_is_charged_in_full() -> None:
    ledger = LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=3600)

    assert ledger.charged_seconds == 3600
    assert ledger.allowance_seconds == 3600


def test_settle_caps_at_reservation() -> None:
    ledger = LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=300)
    ledger = ledger.settle(attempt=1, used_seconds=500)

    assert ledger.charges == (
        AttemptCharge(attempt=1, reserved_seconds=300, settled_seconds=300),
    )
    assert ledger.allowance_seconds == 6900


def test_reserve_beyond_allowance_raises() -> None:
    with pytest.raises(ValueError, match="exceeds allowance"):
        LogicalLedger(cap_seconds=600).reserve(attempt=1, seconds=601)


def test_reserve_while_previous_attempt_unsettled_raises() -> None:
    ledger = LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=300)

    with pytest.raises(ValueError, match="unsettled"):
        ledger.reserve(attempt=2, seconds=300)


def test_reserve_non_increasing_attempt_raises() -> None:
    ledger = LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=300)
    ledger = ledger.settle(attempt=1, used_seconds=100)

    with pytest.raises(ValueError, match="must increase"):
        ledger.reserve(attempt=1, seconds=300)


def test_reserve_zero_seconds_raises() -> None:
    with pytest.raises(ValueError, match="at least 1 second"):
        LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=0)


def test_settle_twice_raises() -> None:
    ledger = LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=300)
    ledger = ledger.settle(attempt=1, used_seconds=100)

    with pytest.raises(ValueError, match="already settled"):
        ledger.settle(attempt=1, used_seconds=100)


def test_settle_unknown_attempt_raises() -> None:
    with pytest.raises(ValueError, match="no reservation"):
        LogicalLedger(cap_seconds=7200).settle(attempt=1, used_seconds=0)


def test_settle_negative_seconds_raises() -> None:
    ledger = LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=300)

    with pytest.raises(ValueError, match="cannot be negative"):
        ledger.settle(attempt=1, used_seconds=-1)


def test_cap_below_one_second_raises() -> None:
    with pytest.raises(ValueError, match="cap_seconds"):
        LogicalLedger(cap_seconds=0)


def _failed(**overrides: Any) -> RunRecord:
    values: dict[str, Any] = {
        "state": RunState.FAILED,
        "failure": failure(FailureCode.HEARTBEAT_LOST),
        "attempt": 1,
        "deadline_at": DEADLINE,
    }
    values.update(overrides)
    return run_record(**values)


def _decide(record: RunRecord, **overrides: Any) -> ContinuationDecision:
    values: dict[str, Any] = {
        "trigger": Trigger.OWNER_RETRY,
        "interruption": None,
        "durable": True,
        "now": NOW,
        "ledger": LogicalLedger(cap_seconds=7200),
        "execution": ExecutionView.TERMINAL_OR_ABSENT,
        "checkpoint": CheckpointView.USABLE,
        "ambiguous_side_effects": False,
        "kill_switch": frozenset(),
        "startup_overhead_seconds": 600,
    }
    values.update(overrides)
    return evaluate_continuation(record, **values)


def _auto(
    record: RunRecord, interruption: Interruption = Interruption.CRASH, **overrides: Any
) -> ContinuationDecision:
    return _decide(record, trigger=Trigger.AUTOMATIC, interruption=interruption, **overrides)


def _exhausted_ledger() -> LogicalLedger:
    return LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=6600).settle(
        attempt=1, used_seconds=6600
    )


def test_owner_retry_allows_next_attempt_with_reservation() -> None:
    ledger = LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=1800)
    ledger = ledger.settle(attempt=1, used_seconds=600)

    decision = _decide(_failed(), ledger=ledger)

    assert decision.verdict is Verdict.ALLOW
    assert decision.reason is None
    assert decision.ledger.charges[-1] == AttemptCharge(attempt=2, reserved_seconds=3600)
    assert decision.ledger.charged_seconds == 4200


def test_allowance_bounds_reservation_below_time_to_deadline() -> None:
    ledger = LogicalLedger(cap_seconds=1000).reserve(attempt=1, seconds=300)
    ledger = ledger.settle(attempt=1, used_seconds=300)

    decision = _decide(_failed(), ledger=ledger)

    assert decision.verdict is Verdict.ALLOW
    assert decision.ledger.charges[-1] == AttemptCharge(attempt=2, reserved_seconds=700)


@pytest.mark.parametrize("seconds_to_deadline", [30, 600])
def test_deadline_limited_time_within_startup_overhead_pauses(
    seconds_to_deadline: int,
) -> None:
    # The ledger has ample allowance, so time to the original deadline is the binding limit.
    ledger = LogicalLedger(cap_seconds=7200)

    decision = _decide(
        _failed(), now=DEADLINE - timedelta(seconds=seconds_to_deadline), ledger=ledger
    )

    assert decision.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert decision.reason is Reason.ALLOWANCE_EXHAUSTED
    assert decision.ledger == ledger


def test_deadline_limited_time_just_above_startup_overhead_allows() -> None:
    decision = _decide(
        _failed(),
        now=DEADLINE - timedelta(seconds=601),
        ledger=LogicalLedger(cap_seconds=7200),
    )

    assert decision.verdict is Verdict.ALLOW
    assert decision.ledger.charges[-1] == AttemptCharge(attempt=2, reserved_seconds=601)


def test_kill_switch_holds_retry_until_reenabled() -> None:
    held = _decide(_failed(), kill_switch=frozenset(KillSwitchClass))

    assert held.verdict is Verdict.HOLD
    assert held.reason is Reason.KILL_SWITCH_ENGAGED
    assert held.ledger == LogicalLedger(cap_seconds=7200)

    allowed = _decide(_failed(), kill_switch=frozenset())

    assert allowed.verdict is Verdict.ALLOW
    assert allowed.ledger.charges == (AttemptCharge(attempt=2, reserved_seconds=3600),)


@pytest.mark.parametrize(
    "state", [RunState.SUCCEEDED, RunState.CANCELLED, RunState.TIMED_OUT]
)
def test_terminal_state_never_restarts(state: RunState) -> None:
    record = run_record(state=state, attempt=1, deadline_at=DEADLINE, failure=None)

    decision = _decide(record, kill_switch=frozenset(KillSwitchClass))

    assert decision.verdict is Verdict.STOP
    assert decision.reason is Reason.TERMINAL_STATE


def test_cancel_requested_never_restarts_even_with_kill_switch_engaged() -> None:
    record = run_record(
        state=RunState.CANCEL_REQUESTED, attempt=1, deadline_at=DEADLINE, failure=None
    )

    decision = _decide(record, kill_switch=frozenset(KillSwitchClass))

    assert decision.verdict is Verdict.STOP
    assert decision.reason is Reason.CANCEL_REQUESTED


@pytest.mark.parametrize(
    ("trigger", "interruption", "engaged", "held"),
    [
        (Trigger.OWNER_RETRY, None, {KillSwitchClass.UNACCEPTED_RETRY_REQUEST}, True),
        (Trigger.OWNER_RETRY, None, {KillSwitchClass.AUTOMATIC_CONTINUATION}, False),
        (
            Trigger.AUTOMATIC,
            Interruption.CRASH,
            {KillSwitchClass.AUTOMATIC_CONTINUATION},
            True,
        ),
        (
            Trigger.AUTOMATIC,
            Interruption.CRASH,
            {KillSwitchClass.UNACCEPTED_RETRY_REQUEST},
            False,
        ),
        # Queued-work classes gate dispatch in the reconciler, not allocation in the policy.
        (
            Trigger.OWNER_RETRY,
            None,
            {
                KillSwitchClass.ADMITTED_QUEUED_WORK,
                KillSwitchClass.ACCEPTED_PENDING_REPLACEMENT,
            },
            False,
        ),
    ],
)
def test_kill_switch_class_holds_only_the_allocation_path_it_names(
    trigger: Trigger,
    interruption: Interruption | None,
    engaged: set[KillSwitchClass],
    held: bool,
) -> None:
    decision = _decide(
        _failed(),
        trigger=trigger,
        interruption=interruption,
        kill_switch=frozenset(engaged),
    )

    if held:
        assert decision.verdict is Verdict.HOLD
        assert decision.reason is Reason.KILL_SWITCH_ENGAGED
        assert decision.ledger == LogicalLedger(cap_seconds=7200)
    else:
        assert decision.verdict is Verdict.ALLOW


@pytest.mark.parametrize(
    "state",
    [
        RunState.AWAITING_UPLOAD,
        RunState.QUEUED,
        RunState.DISPATCHING,
        RunState.STARTING,
        RunState.RUNNING,
        RunState.COLLECTING,
        RunState.RECONCILING,
    ],
)
def test_in_flight_run_is_held_with_its_reservation_not_stopped(state: RunState) -> None:
    # A run that is not failed yet may still be running or queued. Stopping it would end a run
    # that continues, so it holds and its reservation stays counted as capacity.
    ledger = LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=3600)
    record = run_record(state=state, attempt=1, deadline_at=DEADLINE, failure=None)

    decision = _decide(record, ledger=ledger)

    assert decision.verdict is Verdict.HOLD
    assert decision.reason is Reason.RUN_IN_FLIGHT
    assert decision.ledger == ledger


def test_failed_attempt_without_failure_is_not_retryable() -> None:
    decision = _decide(_failed(failure=None))

    assert decision.verdict is Verdict.STOP
    assert decision.reason is Reason.NOT_RETRYABLE


def test_non_retryable_failure_code_stops() -> None:
    decision = _decide(_failed(failure=failure(FailureCode.CHECKSUM_MISMATCH)))

    assert decision.verdict is Verdict.STOP
    assert decision.reason is Reason.NOT_RETRYABLE


def test_deadline_stops_before_kill_switch_hold() -> None:
    decision = _decide(_failed(), now=DEADLINE, kill_switch=frozenset(KillSwitchClass))

    assert decision.verdict is Verdict.STOP
    assert decision.reason is Reason.DEADLINE_EXCEEDED


@pytest.mark.parametrize("execution", [ExecutionView.LIVE, ExecutionView.AMBIGUOUS])
def test_live_or_ambiguous_execution_holds_without_relaunch(execution: ExecutionView) -> None:
    ledger = LogicalLedger(cap_seconds=7200)

    decision = _decide(_failed(), ledger=ledger, execution=execution)

    assert decision.verdict is Verdict.HOLD
    assert decision.reason is Reason.EXECUTION_NOT_TERMINAL
    assert decision.ledger == ledger


def test_execution_hold_takes_precedence_over_kill_switch_reason() -> None:
    decision = _decide(
        _failed(), execution=ExecutionView.LIVE, kill_switch=frozenset(KillSwitchClass)
    )

    assert decision.verdict is Verdict.HOLD
    assert decision.reason is Reason.EXECUTION_NOT_TERMINAL


def test_restart_ceiling_pauses_for_owner() -> None:
    ledger = LogicalLedger(cap_seconds=7200)

    decision = _decide(_failed(attempt=3), ledger=ledger)

    assert decision.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert decision.reason is Reason.RESTART_CEILING
    assert decision.ledger == ledger


def test_restart_ceiling_precedes_exhausted_allowance() -> None:
    decision = _decide(_failed(attempt=3), ledger=_exhausted_ledger())

    assert decision.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert decision.reason is Reason.RESTART_CEILING


def test_deadline_precedes_restart_ceiling() -> None:
    decision = _decide(_failed(attempt=3), now=DEADLINE)

    assert decision.verdict is Verdict.STOP
    assert decision.reason is Reason.DEADLINE_EXCEEDED


def test_exhausted_allowance_at_overhead_boundary_pauses() -> None:
    ledger = _exhausted_ledger()

    decision = _decide(_failed(), ledger=ledger, startup_overhead_seconds=600)

    assert ledger.allowance_seconds == 600
    assert decision.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert decision.reason is Reason.ALLOWANCE_EXHAUSTED
    assert decision.ledger == ledger


@pytest.mark.parametrize(
    ("checkpoint", "reason"),
    [
        (CheckpointView.MISSING, Reason.CHECKPOINT_MISSING),
        (CheckpointView.INCOMPATIBLE, Reason.CHECKPOINT_INCOMPATIBLE),
        (CheckpointView.INTEGRITY_FAILED, Reason.CHECKPOINT_INTEGRITY_FAILED),
    ],
)
def test_unavailable_checkpoint_pauses_for_owner(
    checkpoint: CheckpointView, reason: Reason
) -> None:
    decision = _decide(_failed(), checkpoint=checkpoint)

    assert decision.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert decision.reason is reason


def test_ambiguous_side_effects_pause_for_owner() -> None:
    decision = _decide(_failed(), ambiguous_side_effects=True)

    assert decision.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert decision.reason is Reason.AMBIGUOUS_SIDE_EFFECTS


def test_missing_checkpoint_precedes_ambiguous_side_effects() -> None:
    decision = _decide(
        _failed(), checkpoint=CheckpointView.MISSING, ambiguous_side_effects=True
    )

    assert decision.reason is Reason.CHECKPOINT_MISSING


def test_exhausted_allowance_stays_paused_across_midnight() -> None:
    record = _failed(deadline_at=MIDNIGHT_DEADLINE)
    ledger = _exhausted_ledger()

    before = _decide(record, now=datetime(2026, 10, 21, 23, 59, 59, tzinfo=UTC), ledger=ledger)
    after = _decide(record, now=datetime(2026, 10, 22, 0, 0, 1, tzinfo=UTC), ledger=ledger)

    assert before.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert before.reason is Reason.ALLOWANCE_EXHAUSTED
    assert after.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert after.reason is Reason.ALLOWANCE_EXHAUSTED


def test_reservation_follows_deadline_not_calendar_across_midnight() -> None:
    record = _failed(deadline_at=MIDNIGHT_DEADLINE)
    ledger = LogicalLedger(cap_seconds=7200)

    before = _decide(record, now=datetime(2026, 10, 21, 23, 59, 59, tzinfo=UTC), ledger=ledger)
    after = _decide(record, now=datetime(2026, 10, 22, 0, 0, 1, tzinfo=UTC), ledger=ledger)

    assert before.ledger.charges[-1] == AttemptCharge(attempt=2, reserved_seconds=3601)
    assert after.ledger.charges[-1] == AttemptCharge(attempt=2, reserved_seconds=3599)


@pytest.mark.parametrize(
    "interruption",
    [Interruption.CRASH, Interruption.OOM, Interruption.INFRASTRUCTURE_LOSS],
)
def test_automatic_continuation_allows_interrupted_attempt(interruption: Interruption) -> None:
    ledger = LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=1800)
    ledger = ledger.settle(attempt=1, used_seconds=600)

    decision = _auto(_failed(), interruption, ledger=ledger)

    assert decision.verdict is Verdict.ALLOW
    assert decision.reason is None
    assert decision.ledger.charges[-1] == AttemptCharge(attempt=2, reserved_seconds=3600)
    assert decision.ledger.charged_seconds == 4200


def test_automatic_continuation_stops_for_failure_that_is_not_an_interruption() -> None:
    decision = _decide(_failed(), trigger=Trigger.AUTOMATIC, interruption=None)

    assert decision.verdict is Verdict.STOP
    assert decision.reason is Reason.NOT_AUTOMATIC
    assert decision.ledger == LogicalLedger(cap_seconds=7200)


def test_owner_retry_still_allows_failure_that_is_not_an_interruption() -> None:
    decision = _decide(_failed(), trigger=Trigger.OWNER_RETRY, interruption=None)

    assert decision.verdict is Verdict.ALLOW


def test_owner_retry_ignores_interruption_class() -> None:
    decision = _decide(_failed(), interruption=Interruption.OOM)

    assert decision.verdict is Verdict.ALLOW
    assert decision.ledger.charges[-1] == AttemptCharge(attempt=2, reserved_seconds=3600)


@pytest.mark.parametrize("trigger", list(Trigger))
def test_legacy_or_stateless_run_never_continues(trigger: Trigger) -> None:
    decision = _decide(
        _failed(), trigger=trigger, interruption=Interruption.CRASH, durable=False
    )

    assert decision.verdict is Verdict.STOP
    assert decision.reason is Reason.NOT_DURABLE
    assert decision.ledger == LogicalLedger(cap_seconds=7200)


def test_kill_switch_holds_automatic_continuation_until_reenabled() -> None:
    held = _auto(_failed(), kill_switch=frozenset(KillSwitchClass))

    assert held.verdict is Verdict.HOLD
    assert held.reason is Reason.KILL_SWITCH_ENGAGED
    assert held.ledger == LogicalLedger(cap_seconds=7200)

    allowed = _auto(_failed(), kill_switch=frozenset())

    assert allowed.verdict is Verdict.ALLOW
    assert allowed.ledger.charges == (AttemptCharge(attempt=2, reserved_seconds=3600),)


@pytest.mark.parametrize(
    "state", [RunState.SUCCEEDED, RunState.CANCELLED, RunState.TIMED_OUT]
)
def test_automatic_continuation_never_revives_terminal_run(state: RunState) -> None:
    record = run_record(state=state, attempt=1, deadline_at=DEADLINE, failure=None)

    decision = _auto(record, kill_switch=frozenset(KillSwitchClass))

    assert decision.verdict is Verdict.STOP
    assert decision.reason is Reason.TERMINAL_STATE


def test_automatic_continuation_never_restarts_cancel_requested_run() -> None:
    record = run_record(
        state=RunState.CANCEL_REQUESTED, attempt=1, deadline_at=DEADLINE, failure=None
    )

    decision = _auto(record, kill_switch=frozenset(KillSwitchClass))

    assert decision.verdict is Verdict.STOP
    assert decision.reason is Reason.CANCEL_REQUESTED


def test_automatic_continuation_stops_at_original_deadline() -> None:
    decision = _auto(_failed(), now=DEADLINE, kill_switch=frozenset(KillSwitchClass))

    assert decision.verdict is Verdict.STOP
    assert decision.reason is Reason.DEADLINE_EXCEEDED


@pytest.mark.parametrize("execution", [ExecutionView.LIVE, ExecutionView.AMBIGUOUS])
def test_automatic_continuation_holds_while_execution_not_terminal(
    execution: ExecutionView,
) -> None:
    ledger = LogicalLedger(cap_seconds=7200)

    decision = _auto(_failed(), execution=execution, ledger=ledger)

    assert decision.verdict is Verdict.HOLD
    assert decision.reason is Reason.EXECUTION_NOT_TERMINAL
    assert decision.ledger == ledger


@pytest.mark.parametrize(
    ("checkpoint", "reason"),
    [
        (CheckpointView.MISSING, Reason.CHECKPOINT_MISSING),
        (CheckpointView.INCOMPATIBLE, Reason.CHECKPOINT_INCOMPATIBLE),
        (CheckpointView.INTEGRITY_FAILED, Reason.CHECKPOINT_INTEGRITY_FAILED),
    ],
)
def test_automatic_continuation_pauses_on_unavailable_checkpoint(
    checkpoint: CheckpointView, reason: Reason
) -> None:
    decision = _auto(_failed(), checkpoint=checkpoint)

    assert decision.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert decision.reason is reason
    assert decision.ledger == LogicalLedger(cap_seconds=7200)


def test_crash_loop_pauses_for_owner_at_restart_ceiling() -> None:
    ledger = LogicalLedger(cap_seconds=7200)

    decision = _auto(_failed(attempt=3), Interruption.OOM, ledger=ledger)

    assert decision.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert decision.reason is Reason.RESTART_CEILING
    assert decision.ledger == ledger


def test_automatic_continuation_pauses_on_exhausted_allowance() -> None:
    decision = _auto(_failed(), ledger=_exhausted_ledger())

    assert decision.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert decision.reason is Reason.ALLOWANCE_EXHAUSTED


def test_automatic_continuation_pauses_on_ambiguous_side_effects() -> None:
    decision = _auto(_failed(), ambiguous_side_effects=True)

    assert decision.verdict is Verdict.PAUSED_AWAITING_OWNER
    assert decision.reason is Reason.AMBIGUOUS_SIDE_EFFECTS


def test_exhausted_allowance_stays_paused_across_midnight_for_automatic() -> None:
    record = _failed(deadline_at=MIDNIGHT_DEADLINE)
    ledger = _exhausted_ledger()

    before = _auto(record, now=datetime(2026, 10, 21, 23, 59, 59, tzinfo=UTC), ledger=ledger)
    after = _auto(record, now=datetime(2026, 10, 22, 0, 0, 1, tzinfo=UTC), ledger=ledger)

    assert before.reason is Reason.ALLOWANCE_EXHAUSTED
    assert after.reason is Reason.ALLOWANCE_EXHAUSTED
    assert after.verdict is Verdict.PAUSED_AWAITING_OWNER


def test_automatic_reservation_follows_deadline_not_calendar_across_midnight() -> None:
    record = _failed(deadline_at=MIDNIGHT_DEADLINE)
    ledger = LogicalLedger(cap_seconds=7200)

    before = _auto(record, now=datetime(2026, 10, 21, 23, 59, 59, tzinfo=UTC), ledger=ledger)
    after = _auto(record, now=datetime(2026, 10, 22, 0, 0, 1, tzinfo=UTC), ledger=ledger)

    assert before.ledger.charges[-1] == AttemptCharge(attempt=2, reserved_seconds=3601)
    assert after.ledger.charges[-1] == AttemptCharge(attempt=2, reserved_seconds=3599)
