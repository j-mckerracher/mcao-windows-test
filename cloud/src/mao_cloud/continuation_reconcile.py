"""Continuation reconciler: the single launch gate for a durable logical run.

A Scheduler tick loads the authoritative record, asks Run Admin what the attempt's execution is,
and lets evaluate_continuation decide. Only an ALLOW whose compare-and-set on the record's version
wins becomes a LaunchPermit, so duplicate ticks and late writers cannot open two attempts.

The kill switch is a callable that reconcile and dispatch_gate invoke after their authoritative
load, so an activation during that read is seen before any claim or dispatch. dispatch_gate is the
second gate: a claim winner calls it before jobs.run, and it re-reads the record and the switch.

The store and Run Admin are protocols. Their production implementations belong to DPES-869 (the
durable store and fence contract) and the Cloud Run admin path, neither of which is in this module.
The ledger is returned, never persisted here; the caller owns persistence. The remaining window
between the switch read and the compare-and-set is closed only by the production store, which must
evaluate the switch inside its transaction (DPES-869).
"""

import uuid
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import datetime
from enum import StrEnum
from typing import Protocol

from mao_cloud.continuation import (
    CheckpointView,
    ExecutionView,
    Interruption,
    KillSwitchClass,
    LogicalLedger,
    Reason,
    Trigger,
    Verdict,
    evaluate_continuation,
)
from mao_cloud.protocol import (
    TERMINAL_STATES,
    RunRecord,
    RunState,
    deadline_passed,
    retry,
)

KillSwitchReader = Callable[[], frozenset[KillSwitchClass]]


class ExecutionObservation(StrEnum):
    """What Run Admin reports for one attempt's execution."""

    LIVE = "live"
    TERMINAL = "terminal"
    ABSENT = "absent"
    # Heartbeat stale or lease expired with no termination evidence yet.
    STALE = "stale"
    AMBIGUOUS = "ambiguous"


def execution_view(observation: ExecutionObservation) -> ExecutionView:
    # Stale liveness proves neither running nor ended, so it is held like an unknown state.
    if observation is ExecutionObservation.LIVE:
        return ExecutionView.LIVE
    if observation in (ExecutionObservation.STALE, ExecutionObservation.AMBIGUOUS):
        return ExecutionView.AMBIGUOUS
    return ExecutionView.TERMINAL_OR_ABSENT


@dataclass(frozen=True)
class CheckpointEvidence:
    """What the checkpoint lookup reports. A placeholder shape until DPES-869 defines the contract."""

    present: bool
    compatibility_id: str | None
    integrity_verified: bool


def classify_checkpoint(
    evidence: CheckpointEvidence | None, *, expected_compatibility_id: str
) -> CheckpointView:
    if evidence is None or not evidence.present:
        return CheckpointView.MISSING
    if evidence.compatibility_id != expected_compatibility_id:
        return CheckpointView.INCOMPATIBLE
    if not evidence.integrity_verified:
        return CheckpointView.INTEGRITY_FAILED
    return CheckpointView.USABLE


class RunStore(Protocol):
    """Compare-and-set access to the authoritative run record."""

    def load(self, run_id: str) -> RunRecord: ...

    def compare_and_set(
        self, run_id: str, *, expected_version: int, record: RunRecord
    ) -> bool:
        """Store `record` only if the stored version still equals `expected_version`."""
        ...


class RunAdmin(Protocol):
    """Reports the execution state of one attempt."""

    def observe(self, run_id: str, attempt: int) -> ExecutionObservation: ...


_GATE = object()


@dataclass(frozen=True)
class LaunchPermit:
    """Authority to launch one fresh attempt. Only reconcile() issues one, after its claim wins."""

    run_id: str
    attempt: int
    fence: int
    reserved_seconds: int
    _gate: object = field(repr=False)

    def __post_init__(self) -> None:
        if self._gate is not _GATE:
            raise TypeError(
                "LaunchPermit is issued only by the continuation launch gate"
            )


@dataclass(frozen=True)
class Launch:
    """The claim won. The caller starts exactly this attempt and persists `ledger`."""

    permit: LaunchPermit
    ledger: LogicalLedger


@dataclass(frozen=True)
class LostClaim:
    """Another writer moved the record first. Nothing launches and nothing is reserved."""

    run_id: str
    ledger: LogicalLedger


@dataclass(frozen=True)
class Hold:
    """Execution is not terminal yet. The reservation stays counted as capacity."""

    run_id: str
    reason: Reason
    ledger: LogicalLedger


@dataclass(frozen=True)
class Stop:
    """The run is over for continuation purposes. Its approved reason is visible here."""

    run_id: str
    reason: Reason
    ledger: LogicalLedger


@dataclass(frozen=True)
class PausedAwaitingOwner:
    """The run waits for its owner with an approved reason. No launch is reachable from here."""

    run_id: str
    attempt: int
    reason: Reason
    ledger: LogicalLedger


ContinuationOutcome = Launch | LostClaim | Hold | Stop | PausedAwaitingOwner

_CONTINUATION_NAMESPACE = uuid.uuid5(uuid.NAMESPACE_URL, "mao-cloud:continuation")


def continuation_request_uuid(run_id: str, next_attempt: int) -> uuid.UUID:
    """Same for every tick that opens the same attempt, so a replayed tick is recognised."""
    return uuid.uuid5(_CONTINUATION_NAMESPACE, f"{run_id}:{next_attempt}")


def reconcile(
    store: RunStore,
    admin: RunAdmin,
    run_id: str,
    *,
    trigger: Trigger,
    interruption: Interruption | None,
    durable: bool,
    now: datetime,
    ledger: LogicalLedger,
    checkpoint: CheckpointView,
    ambiguous_side_effects: bool,
    kill_switch: KillSwitchReader,
    startup_overhead_seconds: int,
) -> ContinuationOutcome:
    """One reconciliation tick for a logical run. The only path to a LaunchPermit."""
    record = store.load(run_id)
    engaged = kill_switch()
    observation = admin.observe(run_id, record.attempt)
    decision = evaluate_continuation(
        record,
        trigger=trigger,
        interruption=interruption,
        durable=durable,
        now=now,
        ledger=ledger,
        execution=execution_view(observation),
        checkpoint=checkpoint,
        ambiguous_side_effects=ambiguous_side_effects,
        kill_switch=engaged,
        startup_overhead_seconds=startup_overhead_seconds,
    )
    if decision.verdict is Verdict.ALLOW:
        return _claim(store, record, before=ledger, reserved=decision.ledger, now=now)
    if decision.reason is None:
        raise AssertionError("a non-allow continuation decision must carry a reason")
    if decision.verdict is Verdict.HOLD:
        return Hold(run_id, decision.reason, decision.ledger)
    if decision.verdict is Verdict.STOP:
        return Stop(run_id, decision.reason, decision.ledger)
    return PausedAwaitingOwner(run_id, record.attempt, decision.reason, decision.ledger)


def _claim(
    store: RunStore,
    record: RunRecord,
    *,
    before: LogicalLedger,
    reserved: LogicalLedger,
    now: datetime,
) -> Launch | LostClaim:
    queued = retry(
        record,
        request_uuid=continuation_request_uuid(record.run_id, record.attempt + 1),
        expected_version=record.version,
        expected_attempt=record.attempt,
        now=now,
    )
    if not store.compare_and_set(
        record.run_id, expected_version=record.version, record=queued
    ):
        return LostClaim(record.run_id, before)
    permit = LaunchPermit(
        run_id=queued.run_id,
        attempt=queued.attempt,
        fence=queued.fence,
        reserved_seconds=reserved.charges[-1].reserved_seconds,
        _gate=_GATE,
    )
    return Launch(permit=permit, ledger=reserved)


@dataclass(frozen=True)
class DispatchGo:
    """The queued attempt may call jobs.run. The gate itself wrote nothing."""

    run_id: str
    attempt: int
    fence: int


@dataclass(frozen=True)
class DispatchRefused:
    """The queued attempt must not call jobs.run. HOLD keeps the intent; STOP ends this claim."""

    run_id: str
    verdict: Verdict
    reason: Reason


DispatchOutcome = DispatchGo | DispatchRefused


def _queued_class(attempt: int) -> KillSwitchClass:
    # Attempt one is admitted work; any later attempt is an accepted replacement awaiting dispatch.
    if attempt == 1:
        return KillSwitchClass.ADMITTED_QUEUED_WORK
    return KillSwitchClass.ACCEPTED_PENDING_REPLACEMENT


def dispatch_gate(
    store: RunStore,
    run_id: str,
    *,
    attempt: int,
    fence: int,
    now: datetime,
    kill_switch: KillSwitchReader,
) -> DispatchOutcome:
    """Re-check a claim winner's queued attempt immediately before its jobs.run call.

    The gate reads the record and the switch and never writes, so it cannot move the record out of
    QUEUED. Cancel and terminal states win over every class. A claim whose attempt or fence was
    superseded, or whose attempt is no longer QUEUED, stops and never dispatches. An expired
    deadline stops. Otherwise an engaged class for this attempt's number holds the queued intent
    until the switch is re-enabled.
    """
    record = store.load(run_id)
    engaged = kill_switch()
    if record.state is RunState.CANCEL_REQUESTED:
        return DispatchRefused(run_id, Verdict.STOP, Reason.CANCEL_REQUESTED)
    if record.state in TERMINAL_STATES and record.state is not RunState.FAILED:
        return DispatchRefused(run_id, Verdict.STOP, Reason.TERMINAL_STATE)
    if (
        record.attempt != attempt
        or record.fence != fence
        or record.state is not RunState.QUEUED
    ):
        return DispatchRefused(run_id, Verdict.STOP, Reason.STALE_CLAIM)
    if deadline_passed(now, record.deadline_at):
        return DispatchRefused(run_id, Verdict.STOP, Reason.DEADLINE_EXCEEDED)
    if _queued_class(attempt) in engaged:
        return DispatchRefused(run_id, Verdict.HOLD, Reason.KILL_SWITCH_ENGAGED)
    return DispatchGo(run_id=run_id, attempt=attempt, fence=fence)
