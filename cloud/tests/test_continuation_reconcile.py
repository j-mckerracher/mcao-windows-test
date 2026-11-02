"""Continuation reconciler: CAS claim, Run Admin states, race fixtures, launch gate, pause outcome.

Every store and Run Admin result here comes from continuation_fakes, which is FAKE. They prove the
claim logic and the policy against in-memory state. DPES-869's real store and the live Cloud Run
admin path are not exercised, and the checkpoint shape is a placeholder until DPES-869 lands.
"""

import threading
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from datetime import UTC, datetime, timedelta
from typing import Any

import pytest

from continuation_fakes import FakeRunAdmin, FakeRunStore
from fakes import RUN_ID, run_record
from mao_cloud.continuation import (
    CheckpointView,
    Interruption,
    KillSwitchClass,
    LogicalLedger,
    Reason,
    Trigger,
    Verdict,
)
from mao_cloud.continuation_reconcile import (
    CheckpointEvidence,
    DispatchGo,
    DispatchRefused,
    ExecutionObservation,
    Hold,
    Launch,
    LaunchPermit,
    LostClaim,
    PausedAwaitingOwner,
    Stop,
    classify_checkpoint,
    dispatch_gate,
    reconcile,
)
from mao_cloud.protocol import FailureCode, RunRecord, RunState, failure

NOW = datetime(2026, 10, 22, 12, 0, tzinfo=UTC)
DEADLINE = NOW + timedelta(seconds=3600)
MIDNIGHT_DEADLINE = datetime(2026, 10, 22, 1, 0, tzinfo=UTC)
COMPATIBILITY_ID = "compat-v1"
DUPLICATE_TICKS = 8


def _dead_attempt(**overrides: Any) -> RunRecord:
    """Attempt 1 failed retryably with its heartbeat lost: what a Scheduler tick reads."""
    values: dict[str, Any] = {
        "state": RunState.FAILED,
        "failure": failure(FailureCode.HEARTBEAT_LOST),
        "attempt": 1,
        "version": 5,
        "deadline_at": DEADLINE,
        "last_seen_at": NOW - timedelta(hours=2),
    }
    values.update(overrides)
    return run_record(**values)


def _landed(record: RunRecord, **changes: Any) -> RunRecord:
    """The record another writer leaves behind: one version later, with the given changes."""
    return record.model_copy(update={"version": record.version + 1, **changes})


def _settled_ledger(used_seconds: int = 600) -> LogicalLedger:
    return (
        LogicalLedger(cap_seconds=7200)
        .reserve(attempt=1, seconds=3600)
        .settle(attempt=1, used_seconds=used_seconds)
    )


def _exhausted_ledger() -> LogicalLedger:
    return (
        LogicalLedger(cap_seconds=7200)
        .reserve(attempt=1, seconds=7200)
        .settle(attempt=1, used_seconds=7200)
    )


def _tick(store: FakeRunStore, admin: FakeRunAdmin, **overrides: Any) -> Any:
    """One automatic Scheduler reconciliation of RUN_ID with defaults that allow a replacement."""
    values: dict[str, Any] = {
        "trigger": Trigger.AUTOMATIC,
        "interruption": Interruption.CRASH,
        "durable": True,
        "now": NOW,
        "ledger": _settled_ledger(),
        "checkpoint": CheckpointView.USABLE,
        "ambiguous_side_effects": False,
        "kill_switch": lambda: frozenset(),
        "startup_overhead_seconds": 600,
    }
    values.update(overrides)
    return reconcile(store, admin, RUN_ID, **values)


# AC1 and T1: one claim winner per dead attempt, decided by compare-and-set.


def test_duplicate_ticks_race_to_one_claim_winner() -> None:
    barrier = threading.Barrier(DUPLICATE_TICKS, timeout=10)
    store = FakeRunStore(_dead_attempt())
    store.set_after_load(lambda: barrier.wait())
    admin = FakeRunAdmin(ExecutionObservation.ABSENT)

    with ThreadPoolExecutor(max_workers=DUPLICATE_TICKS) as pool:
        outcomes = list(pool.map(lambda _: _tick(store, admin), range(DUPLICATE_TICKS)))

    launches = [outcome for outcome in outcomes if isinstance(outcome, Launch)]
    losers = [outcome for outcome in outcomes if isinstance(outcome, LostClaim)]
    assert len(launches) == 1
    assert len(losers) == DUPLICATE_TICKS - 1
    assert all(loser.ledger == _settled_ledger() for loser in losers)
    assert store.compare_and_set_calls == DUPLICATE_TICKS
    assert store.current.attempt == 2
    assert store.current.fence == 2
    assert store.current.state is RunState.QUEUED
    assert launches[0].permit.attempt == 2


def test_tick_after_claim_holds_the_queued_launch_without_a_second_launch() -> None:
    store = FakeRunStore(_dead_attempt())
    admin = FakeRunAdmin(ExecutionObservation.ABSENT)

    first = _tick(store, admin)
    # The caller persists the ledger its Launch returned and passes it to the next tick.
    second = _tick(store, admin, ledger=first.ledger)

    assert isinstance(first, Launch)
    # The queued attempt is still in flight: its launch belongs to the claim winner, so it holds
    # with its persisted reservation rather than being reported as a stopped run.
    assert isinstance(second, Hold)
    assert second.reason is Reason.RUN_IN_FLIGHT
    assert second.ledger == first.ledger
    assert store.current.attempt == 2
    assert store.compare_and_set_calls == 1


@pytest.mark.parametrize(
    "observation",
    [
        ExecutionObservation.LIVE,
        ExecutionObservation.STALE,
        ExecutionObservation.AMBIGUOUS,
        ExecutionObservation.ABSENT,
        ExecutionObservation.TERMINAL,
    ],
)
def test_queued_launch_intent_holds_for_every_observation(
    observation: ExecutionObservation,
) -> None:
    # A jobs.run whose response was lost leaves QUEUED with an unknown execution. That must hold
    # its reservation, never end the run and never open another attempt.
    queued = run_record(
        state=RunState.QUEUED,
        attempt=2,
        fence=2,
        failure=None,
        deadline_at=DEADLINE,
        version=6,
    )
    store = FakeRunStore(queued)

    outcome = _tick(store, FakeRunAdmin(observation))

    assert isinstance(outcome, Hold)
    assert outcome.reason is Reason.RUN_IN_FLIGHT
    assert not isinstance(outcome, Stop)
    assert store.compare_and_set_calls == 0
    assert store.current == queued


@pytest.mark.parametrize(
    ("trigger", "interruption", "engaged", "held"),
    [
        (Trigger.AUTOMATIC, Interruption.CRASH, {KillSwitchClass.AUTOMATIC_CONTINUATION}, True),
        (Trigger.AUTOMATIC, Interruption.CRASH, {KillSwitchClass.UNACCEPTED_RETRY_REQUEST}, False),
        (Trigger.OWNER_RETRY, None, {KillSwitchClass.UNACCEPTED_RETRY_REQUEST}, True),
        (Trigger.OWNER_RETRY, None, {KillSwitchClass.AUTOMATIC_CONTINUATION}, False),
    ],
)
def test_kill_switch_class_blocks_only_its_allocation_path(
    trigger: Trigger,
    interruption: Interruption | None,
    engaged: set[KillSwitchClass],
    held: bool,
) -> None:
    start = _dead_attempt()
    store = FakeRunStore(start)

    outcome = _tick(
        store,
        FakeRunAdmin(ExecutionObservation.ABSENT),
        trigger=trigger,
        interruption=interruption,
        kill_switch=lambda: frozenset(engaged),
    )

    if held:
        assert isinstance(outcome, Hold)
        assert outcome.reason is Reason.KILL_SWITCH_ENGAGED
        assert store.compare_and_set_calls == 0
        assert store.current == start
    else:
        assert isinstance(outcome, Launch)


def test_kill_switch_is_read_after_the_record_load_before_any_claim() -> None:
    # Activation between the authoritative read and the allocation must still hold the claim.
    engaged: set[KillSwitchClass] = set()
    store = FakeRunStore(_dead_attempt())
    store.set_after_load(lambda: engaged.add(KillSwitchClass.AUTOMATIC_CONTINUATION))

    outcome = _tick(
        store,
        FakeRunAdmin(ExecutionObservation.ABSENT),
        kill_switch=lambda: frozenset(engaged),
    )

    assert isinstance(outcome, Hold)
    assert outcome.reason is Reason.KILL_SWITCH_ENGAGED
    assert store.compare_and_set_calls == 0


# AC2 and T1: fake Run Admin states decide hold, allow or stop, and a hold keeps capacity.


@pytest.mark.parametrize(
    "observation",
    [
        ExecutionObservation.LIVE,
        ExecutionObservation.STALE,
        ExecutionObservation.AMBIGUOUS,
    ],
)
def test_live_stale_or_ambiguous_execution_holds_without_replacement(
    observation: ExecutionObservation,
) -> None:
    # A stale heartbeat or expired lease alone never opens a replacement; only termination does.
    start = _dead_attempt()
    store = FakeRunStore(start)
    admin = FakeRunAdmin(observation)

    outcome = _tick(store, admin)

    assert isinstance(outcome, Hold)
    assert outcome.reason is Reason.EXECUTION_NOT_TERMINAL
    assert store.compare_and_set_calls == 0
    assert store.current == start
    assert admin.observations == [(RUN_ID, 1)]


def test_hold_keeps_the_unsettled_reservation_as_capacity() -> None:
    reserved = LogicalLedger(cap_seconds=7200).reserve(attempt=1, seconds=3600)
    store = FakeRunStore(_dead_attempt())

    outcome = _tick(store, FakeRunAdmin(ExecutionObservation.LIVE), ledger=reserved)

    assert isinstance(outcome, Hold)
    assert outcome.ledger == reserved
    assert outcome.ledger.charged_seconds == 3600
    assert outcome.ledger.allowance_seconds == 3600


@pytest.mark.parametrize(
    "observation", [ExecutionObservation.TERMINAL, ExecutionObservation.ABSENT]
)
def test_terminal_or_absent_execution_allows_exactly_one_replacement(
    observation: ExecutionObservation,
) -> None:
    store = FakeRunStore(_dead_attempt())

    outcome = _tick(store, FakeRunAdmin(observation))

    assert isinstance(outcome, Launch)
    assert store.current.attempt == 2
    assert store.compare_and_set_calls == 1


# AC4 and T3: cancellation, success and late writes never revive a run.


def _landed_cancel_on_new_attempt(start: RunRecord) -> RunRecord:
    return _landed(
        start,
        state=RunState.CANCELLED,
        attempt=2,
        fence=2,
        failure=failure(FailureCode.OWNER_CANCELLED),
    )


def _landed_success_on_new_attempt(start: RunRecord) -> RunRecord:
    return _landed(start, state=RunState.SUCCEEDED, attempt=2, fence=2, failure=None)


@pytest.mark.parametrize(
    "landed",
    [
        pytest.param(lambda start: _landed(start), id="late-checkpoint-same-attempt"),
        pytest.param(_landed_cancel_on_new_attempt, id="owner-cancel-after-claim"),
        pytest.param(_landed_success_on_new_attempt, id="success-after-claim"),
    ],
)
def test_write_landing_between_read_and_claim_blocks_the_stale_replacement(
    landed: Callable[[RunRecord], RunRecord],
) -> None:
    start = _dead_attempt()
    store = FakeRunStore(start)
    store.set_after_load(lambda: store.land_other_writer(landed(start)))
    admin = FakeRunAdmin(ExecutionObservation.ABSENT)

    outcome = _tick(store, admin)

    assert isinstance(outcome, LostClaim)
    assert store.compare_and_set_calls == 1
    assert store.current == landed(start)


@pytest.mark.parametrize(
    ("state", "owner_failure", "reason"),
    [
        (
            RunState.CANCEL_REQUESTED,
            FailureCode.OWNER_CANCELLED,
            Reason.CANCEL_REQUESTED,
        ),
        (RunState.CANCELLED, FailureCode.OWNER_CANCELLED, Reason.TERMINAL_STATE),
        (RunState.SUCCEEDED, None, Reason.TERMINAL_STATE),
    ],
)
def test_cancelled_or_succeeded_run_is_never_revived(
    state: RunState, owner_failure: FailureCode | None, reason: Reason
) -> None:
    start = _dead_attempt(
        state=state,
        failure=failure(owner_failure) if owner_failure is not None else None,
        last_seen_at=NOW,
    )
    store = FakeRunStore(start)
    admin = FakeRunAdmin(ExecutionObservation.ABSENT)

    outcome = _tick(store, admin)

    assert isinstance(outcome, Stop)
    assert outcome.reason is reason
    assert store.compare_and_set_calls == 0
    assert store.current == start


# AC5 and T4: one launch gate refuses on deadline, budget, restart ceiling and midnight.


def test_launch_permit_is_issued_only_by_the_gate() -> None:
    with pytest.raises(TypeError, match="issued only by the continuation launch gate"):
        LaunchPermit(
            run_id=RUN_ID, attempt=2, fence=2, reserved_seconds=60, _gate=object()
        )


def test_deadline_refuses_launch_without_writing() -> None:
    start = _dead_attempt(deadline_at=NOW)
    store = FakeRunStore(start)

    outcome = _tick(store, FakeRunAdmin(ExecutionObservation.ABSENT), now=NOW)

    assert isinstance(outcome, Stop)
    assert outcome.reason is Reason.DEADLINE_EXCEEDED
    assert store.compare_and_set_calls == 0
    assert store.current == start


def test_exhausted_budget_pauses_with_reason_and_no_launch() -> None:
    start = _dead_attempt()
    store = FakeRunStore(start)

    outcome = _tick(
        store, FakeRunAdmin(ExecutionObservation.ABSENT), ledger=_exhausted_ledger()
    )

    assert isinstance(outcome, PausedAwaitingOwner)
    assert outcome.reason is Reason.ALLOWANCE_EXHAUSTED
    assert store.compare_and_set_calls == 0
    assert store.current == start


def test_restart_ceiling_pauses_with_reason_and_no_launch() -> None:
    start = _dead_attempt(attempt=3)
    store = FakeRunStore(start)

    outcome = _tick(store, FakeRunAdmin(ExecutionObservation.ABSENT))

    assert isinstance(outcome, PausedAwaitingOwner)
    assert outcome.reason is Reason.RESTART_CEILING
    assert outcome.attempt == 3
    assert store.compare_and_set_calls == 0


def test_midnight_does_not_restore_an_exhausted_budget() -> None:
    start = _dead_attempt(deadline_at=MIDNIGHT_DEADLINE)
    ledger = _exhausted_ledger()

    before_store = FakeRunStore(start)
    before = _tick(
        before_store,
        FakeRunAdmin(ExecutionObservation.ABSENT),
        now=datetime(2026, 10, 21, 23, 59, 59, tzinfo=UTC),
        ledger=ledger,
    )
    after_store = FakeRunStore(start)
    after = _tick(
        after_store,
        FakeRunAdmin(ExecutionObservation.ABSENT),
        now=datetime(2026, 10, 22, 0, 0, 1, tzinfo=UTC),
        ledger=ledger,
    )

    assert isinstance(before, PausedAwaitingOwner)
    assert isinstance(after, PausedAwaitingOwner)
    assert before.reason is after.reason is Reason.ALLOWANCE_EXHAUSTED
    assert before_store.compare_and_set_calls == after_store.compare_and_set_calls == 0


# T6 and AC7: checkpoint fixtures classify, and unrecoverable runs pause with a typed reason.


@pytest.mark.parametrize(
    ("evidence", "expected"),
    [
        (None, CheckpointView.MISSING),
        (
            CheckpointEvidence(
                present=False, compatibility_id=None, integrity_verified=False
            ),
            CheckpointView.MISSING,
        ),
        (
            CheckpointEvidence(
                present=True, compatibility_id="compat-v0", integrity_verified=True
            ),
            CheckpointView.INCOMPATIBLE,
        ),
        (
            CheckpointEvidence(
                present=True,
                compatibility_id=COMPATIBILITY_ID,
                integrity_verified=False,
            ),
            CheckpointView.INTEGRITY_FAILED,
        ),
        (
            CheckpointEvidence(
                present=True, compatibility_id=COMPATIBILITY_ID, integrity_verified=True
            ),
            CheckpointView.USABLE,
        ),
    ],
)
def test_checkpoint_fixture_verdicts(
    evidence: CheckpointEvidence | None, expected: CheckpointView
) -> None:
    assert (
        classify_checkpoint(evidence, expected_compatibility_id=COMPATIBILITY_ID)
        is expected
    )


@pytest.mark.parametrize(
    ("evidence", "ambiguous", "reason"),
    [
        (None, False, Reason.CHECKPOINT_MISSING),
        (
            CheckpointEvidence(
                present=True, compatibility_id="compat-v0", integrity_verified=True
            ),
            False,
            Reason.CHECKPOINT_INCOMPATIBLE,
        ),
        (
            CheckpointEvidence(
                present=True, compatibility_id=COMPATIBILITY_ID, integrity_verified=True
            ),
            True,
            Reason.AMBIGUOUS_SIDE_EFFECTS,
        ),
    ],
)
def test_unrecoverable_run_pauses_for_owner_and_never_launches(
    evidence: CheckpointEvidence | None, ambiguous: bool, reason: Reason
) -> None:
    start = _dead_attempt()
    store = FakeRunStore(start)
    checkpoint = classify_checkpoint(
        evidence, expected_compatibility_id=COMPATIBILITY_ID
    )

    outcome = _tick(
        store,
        FakeRunAdmin(ExecutionObservation.ABSENT),
        checkpoint=checkpoint,
        ambiguous_side_effects=ambiguous,
    )

    assert isinstance(outcome, PausedAwaitingOwner)
    assert not isinstance(outcome, Launch)
    assert outcome.run_id == RUN_ID
    assert outcome.attempt == 1
    assert outcome.reason is reason
    assert outcome.ledger == _settled_ledger()
    assert store.compare_and_set_calls == 0
    assert store.current == start


# Dispatch gate: the claim winner re-reads the record and the switch before it calls jobs.run.


def _queued(attempt: int, **overrides: Any) -> RunRecord:
    """A launch intent persisted by a claim (or admission for attempt 1), not yet dispatched."""
    values: dict[str, Any] = {
        "state": RunState.QUEUED,
        "attempt": attempt,
        "fence": attempt,
        "failure": None,
        "deadline_at": DEADLINE,
        "version": 6,
    }
    values.update(overrides)
    return run_record(**values)


def _claimed_replacement(store: FakeRunStore) -> None:
    assert isinstance(_tick(store, FakeRunAdmin(ExecutionObservation.ABSENT)), Launch)


def test_claim_winner_dispatches_when_no_class_is_engaged() -> None:
    store = FakeRunStore(_dead_attempt())
    _claimed_replacement(store)

    outcome = dispatch_gate(
        store, RUN_ID, attempt=2, fence=2, now=NOW, kill_switch=lambda: frozenset()
    )

    assert outcome == DispatchGo(run_id=RUN_ID, attempt=2, fence=2)
    # The gate only reads: the single write in this scenario is the claim's.
    assert store.compare_and_set_calls == 1


@pytest.mark.parametrize(
    ("attempt", "engaged_class"),
    [
        (1, KillSwitchClass.ADMITTED_QUEUED_WORK),
        (2, KillSwitchClass.ACCEPTED_PENDING_REPLACEMENT),
    ],
)
def test_engaged_class_holds_queued_work_before_dispatch(
    attempt: int, engaged_class: KillSwitchClass
) -> None:
    queued = _queued(attempt)
    store = FakeRunStore(queued)

    outcome = dispatch_gate(
        store,
        RUN_ID,
        attempt=attempt,
        fence=attempt,
        now=NOW,
        kill_switch=lambda: frozenset({engaged_class}),
    )

    assert isinstance(outcome, DispatchRefused)
    assert outcome.verdict is Verdict.HOLD
    assert outcome.reason is Reason.KILL_SWITCH_ENGAGED
    assert store.current == queued
    assert store.compare_and_set_calls == 0


@pytest.mark.parametrize(
    ("attempt", "other_class"),
    [
        (1, KillSwitchClass.ACCEPTED_PENDING_REPLACEMENT),
        (2, KillSwitchClass.ADMITTED_QUEUED_WORK),
    ],
)
def test_other_queued_class_does_not_hold_this_attempt(
    attempt: int, other_class: KillSwitchClass
) -> None:
    store = FakeRunStore(_queued(attempt))

    outcome = dispatch_gate(
        store,
        RUN_ID,
        attempt=attempt,
        fence=attempt,
        now=NOW,
        kill_switch=lambda: frozenset({other_class}),
    )

    assert outcome == DispatchGo(run_id=RUN_ID, attempt=attempt, fence=attempt)


def test_switch_engaged_after_claim_holds_until_reenabled_then_dispatches() -> None:
    store = FakeRunStore(_dead_attempt())
    _claimed_replacement(store)
    engaged: set[KillSwitchClass] = {KillSwitchClass.ACCEPTED_PENDING_REPLACEMENT}

    def reader() -> frozenset[KillSwitchClass]:
        return frozenset(engaged)

    held = dispatch_gate(store, RUN_ID, attempt=2, fence=2, now=NOW, kill_switch=reader)
    engaged.clear()
    released = dispatch_gate(store, RUN_ID, attempt=2, fence=2, now=NOW, kill_switch=reader)

    assert isinstance(held, DispatchRefused)
    assert held.reason is Reason.KILL_SWITCH_ENGAGED
    assert released == DispatchGo(run_id=RUN_ID, attempt=2, fence=2)


def test_switch_activated_during_the_dispatch_read_is_seen() -> None:
    # The switch is read after the record load, so activation inside that window still holds.
    engaged: set[KillSwitchClass] = set()
    store = FakeRunStore(_queued(2))
    store.set_after_load(lambda: engaged.add(KillSwitchClass.ACCEPTED_PENDING_REPLACEMENT))

    outcome = dispatch_gate(
        store,
        RUN_ID,
        attempt=2,
        fence=2,
        now=NOW,
        kill_switch=lambda: frozenset(engaged),
    )

    assert isinstance(outcome, DispatchRefused)
    assert outcome.reason is Reason.KILL_SWITCH_ENGAGED


@pytest.mark.parametrize("attempt", [1, 2])
@pytest.mark.parametrize(
    ("state", "reason"),
    [
        (RunState.CANCEL_REQUESTED, Reason.CANCEL_REQUESTED),
        (RunState.CANCELLED, Reason.TERMINAL_STATE),
        (RunState.SUCCEEDED, Reason.TERMINAL_STATE),
        (RunState.TIMED_OUT, Reason.TERMINAL_STATE),
    ],
)
def test_cancel_and_terminal_win_over_every_dispatch_class(
    attempt: int, state: RunState, reason: Reason
) -> None:
    store = FakeRunStore(_queued(attempt, state=state))

    outcome = dispatch_gate(
        store,
        RUN_ID,
        attempt=attempt,
        fence=attempt,
        now=NOW,
        kill_switch=lambda: frozenset(KillSwitchClass),
    )

    assert isinstance(outcome, DispatchRefused)
    assert outcome.verdict is Verdict.STOP
    assert outcome.reason is reason
    assert store.compare_and_set_calls == 0


@pytest.mark.parametrize(
    "record",
    [
        pytest.param(_queued(2, fence=3), id="fence-moved-by-another-writer"),
        pytest.param(
            _queued(2, state=RunState.FAILED, failure=failure(FailureCode.HEARTBEAT_LOST)),
            id="attempt-failed-before-dispatch",
        ),
    ],
)
def test_superseded_claim_never_dispatches(record: RunRecord) -> None:
    store = FakeRunStore(record)

    outcome = dispatch_gate(
        store, RUN_ID, attempt=2, fence=2, now=NOW, kill_switch=lambda: frozenset()
    )

    assert isinstance(outcome, DispatchRefused)
    assert outcome.verdict is Verdict.STOP
    assert outcome.reason is Reason.STALE_CLAIM


def test_deadline_passed_between_claim_and_dispatch_refuses_without_writing() -> None:
    store = FakeRunStore(_queued(2, deadline_at=NOW))

    outcome = dispatch_gate(
        store, RUN_ID, attempt=2, fence=2, now=NOW, kill_switch=lambda: frozenset()
    )

    assert isinstance(outcome, DispatchRefused)
    assert outcome.verdict is Verdict.STOP
    assert outcome.reason is Reason.DEADLINE_EXCEEDED
    assert store.compare_and_set_calls == 0
