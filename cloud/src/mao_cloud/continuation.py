"""Pure continuation policy for a durable logical run.

A logical run spans its attempts. Two triggers may open the next attempt: automatic continuation
after a crash, OOM or infrastructure loss, and an explicit owner retry, which stays the manual
path. Both share one evaluation order and one per-run reserve/settle ledger, so the original
deadline, restart ceiling and runtime allowance bound every attempt. This module defines no
state, schema, or fence: attempt and fence identity come from protocol.py and the DPES-869
checkpoint contract.

Only durable logical runs continue. Legacy and stateless runs stop here and keep their existing
retry behaviour. The checkpoint view is only a usability check: every attempt opens fresh, and
nothing here restores a checkpoint, which waits on the DPES-869 contract. A missing, incompatible
or integrity-failed checkpoint pauses for the owner and is never replaced by a stateless run. A
crash loop is bounded by the restart ceiling and pauses for the owner.

Nothing in this module reads the clock or mutates state. Callers persist the returned ledger.
"""

import math
from dataclasses import dataclass, replace
from datetime import datetime
from enum import StrEnum

from mao_cloud.protocol import TERMINAL_STATES, RunRecord, RunState, deadline_passed


class ExecutionView(StrEnum):
    TERMINAL_OR_ABSENT = "terminal_or_absent"
    LIVE = "live"
    AMBIGUOUS = "ambiguous"


class CheckpointView(StrEnum):
    """Checkpoint state as the caller sees it. Non-usable values mirror DPES-869's error codes."""

    USABLE = "usable"
    MISSING = "checkpoint_missing"
    INCOMPATIBLE = "checkpoint_incompatible"
    INTEGRITY_FAILED = "checkpoint_integrity_failed"


class Trigger(StrEnum):
    AUTOMATIC = "automatic"
    OWNER_RETRY = "owner_retry"


class Interruption(StrEnum):
    """Why a retryable attempt ended, as the caller classified its execution evidence."""

    CRASH = "crash"
    OOM = "oom"
    INFRASTRUCTURE_LOSS = "infrastructure_loss"


class Verdict(StrEnum):
    ALLOW = "allow"
    HOLD = "hold"
    STOP = "stop"
    PAUSED_AWAITING_OWNER = "paused_awaiting_owner"


class KillSwitchClass(StrEnum):
    """Work a kill switch stops. Each class names the one path it blocks.

    The two allocation classes gate whether the reconciler opens a new attempt. The two queued
    classes gate whether a queued attempt is dispatched, and apply to the attempt's number: attempt
    one is admitted work, and a later attempt is an accepted replacement waiting for dispatch.
    """

    ADMITTED_QUEUED_WORK = "admitted_queued_work"
    UNACCEPTED_RETRY_REQUEST = "unaccepted_retry_request"
    ACCEPTED_PENDING_REPLACEMENT = "accepted_pending_replacement"
    AUTOMATIC_CONTINUATION = "automatic_continuation"


class Reason(StrEnum):
    NOT_RETRYABLE = "not_retryable"
    NOT_DURABLE = "not_durable"
    NOT_AUTOMATIC = "not_automatic"
    CANCEL_REQUESTED = "cancel_requested"
    TERMINAL_STATE = "terminal_state"
    DEADLINE_EXCEEDED = "deadline_exceeded"
    RUN_IN_FLIGHT = "run_in_flight"
    EXECUTION_NOT_TERMINAL = "execution_not_terminal"
    KILL_SWITCH_ENGAGED = "kill_switch_engaged"
    STALE_CLAIM = "stale_claim"
    RESTART_CEILING = "restart_ceiling"
    ALLOWANCE_EXHAUSTED = "allowance_exhausted"
    CHECKPOINT_MISSING = "checkpoint_missing"
    CHECKPOINT_INCOMPATIBLE = "checkpoint_incompatible"
    CHECKPOINT_INTEGRITY_FAILED = "checkpoint_integrity_failed"
    AMBIGUOUS_SIDE_EFFECTS = "ambiguous_side_effects"


@dataclass(frozen=True)
class AttemptCharge:
    """One attempt's runtime charge. An unsettled charge counts as its full reservation."""

    attempt: int
    reserved_seconds: int
    settled_seconds: int | None = None

    @property
    def charged_seconds(self) -> int:
        if self.settled_seconds is None:
            return self.reserved_seconds
        return self.settled_seconds


@dataclass(frozen=True)
class LogicalLedger:
    """Runtime allowance for a whole logical run, reserved and settled per attempt.

    The ledger has no calendar key: the allowance never resets at a day boundary.
    """

    cap_seconds: int
    charges: tuple[AttemptCharge, ...] = ()

    def __post_init__(self) -> None:
        if self.cap_seconds < 1:
            raise ValueError("cap_seconds must be at least 1")

    @property
    def charged_seconds(self) -> int:
        return sum(charge.charged_seconds for charge in self.charges)

    @property
    def allowance_seconds(self) -> int:
        return max(0, self.cap_seconds - self.charged_seconds)

    def reserve(self, *, attempt: int, seconds: int) -> "LogicalLedger":
        if seconds < 1:
            raise ValueError("reservation must be at least 1 second")
        if self.charges and self.charges[-1].settled_seconds is None:
            raise ValueError("previous attempt is unsettled")
        if self.charges and attempt <= self.charges[-1].attempt:
            raise ValueError("attempt must increase")
        if seconds > self.allowance_seconds:
            raise ValueError("reservation exceeds allowance")
        charge = AttemptCharge(attempt=attempt, reserved_seconds=seconds)
        return replace(self, charges=(*self.charges, charge))

    def settle(self, *, attempt: int, used_seconds: int) -> "LogicalLedger":
        if used_seconds < 0:
            raise ValueError("used_seconds cannot be negative")
        for index, charge in enumerate(self.charges):
            if charge.attempt != attempt:
                continue
            if charge.settled_seconds is not None:
                raise ValueError("attempt already settled")
            settled = replace(
                charge, settled_seconds=min(used_seconds, charge.reserved_seconds)
            )
            charges = (*self.charges[:index], settled, *self.charges[index + 1 :])
            return replace(self, charges=charges)
        raise ValueError("no reservation for attempt")


@dataclass(frozen=True)
class ContinuationDecision:
    """Outcome of one owner retry. The ledger is reserved only on ALLOW, else unchanged."""

    verdict: Verdict
    reason: Reason | None
    ledger: LogicalLedger


def _stop(ledger: LogicalLedger, reason: Reason) -> ContinuationDecision:
    return ContinuationDecision(Verdict.STOP, reason, ledger)


def _hold(ledger: LogicalLedger, reason: Reason) -> ContinuationDecision:
    return ContinuationDecision(Verdict.HOLD, reason, ledger)


def _pause(ledger: LogicalLedger, reason: Reason) -> ContinuationDecision:
    return ContinuationDecision(Verdict.PAUSED_AWAITING_OWNER, reason, ledger)


def evaluate_continuation(
    record: RunRecord,
    *,
    trigger: Trigger,
    interruption: Interruption | None,
    durable: bool,
    now: datetime,
    ledger: LogicalLedger,
    execution: ExecutionView,
    checkpoint: CheckpointView,
    ambiguous_side_effects: bool,
    kill_switch: frozenset[KillSwitchClass],
    startup_overhead_seconds: int,
) -> ContinuationDecision:
    """Decide whether attempt record.attempt + 1 may open for this logical run.

    AUTOMATIC continues only after an interruption and stops for any other retryable failure,
    which leaves the owner retry. OWNER_RETRY ignores interruption. The caller settles the failed
    attempt's charge before calling; an unsettled charge makes the reservation raise. Rules run
    in order and the first match wins. Cancel and terminal state are checked before any kill-switch
    class, so they win over every class. A run that is not failed yet is still in flight and holds.
    """
    if record.state is RunState.CANCEL_REQUESTED:
        return _stop(ledger, Reason.CANCEL_REQUESTED)
    if record.state in TERMINAL_STATES and record.state is not RunState.FAILED:
        return _stop(ledger, Reason.TERMINAL_STATE)
    if record.state is not RunState.FAILED:
        return _hold(ledger, Reason.RUN_IN_FLIGHT)
    if record.failure is None or not record.failure.retryable:
        return _stop(ledger, Reason.NOT_RETRYABLE)
    if not durable:
        return _stop(ledger, Reason.NOT_DURABLE)
    if trigger is Trigger.AUTOMATIC and interruption is None:
        return _stop(ledger, Reason.NOT_AUTOMATIC)
    if deadline_passed(now, record.deadline_at):
        return _stop(ledger, Reason.DEADLINE_EXCEEDED)
    if execution is not ExecutionView.TERMINAL_OR_ABSENT:
        return _hold(ledger, Reason.EXECUTION_NOT_TERMINAL)
    allocation_class = (
        KillSwitchClass.AUTOMATIC_CONTINUATION
        if trigger is Trigger.AUTOMATIC
        else KillSwitchClass.UNACCEPTED_RETRY_REQUEST
    )
    if allocation_class in kill_switch:
        return _hold(ledger, Reason.KILL_SWITCH_ENGAGED)
    if record.attempt >= record.max_attempts:
        return _pause(ledger, Reason.RESTART_CEILING)
    if min(
        ledger.allowance_seconds, (record.deadline_at - now).total_seconds()
    ) <= startup_overhead_seconds:
        return _pause(ledger, Reason.ALLOWANCE_EXHAUSTED)
    if checkpoint is CheckpointView.MISSING:
        return _pause(ledger, Reason.CHECKPOINT_MISSING)
    if checkpoint is CheckpointView.INCOMPATIBLE:
        return _pause(ledger, Reason.CHECKPOINT_INCOMPATIBLE)
    if checkpoint is CheckpointView.INTEGRITY_FAILED:
        return _pause(ledger, Reason.CHECKPOINT_INTEGRITY_FAILED)
    if ambiguous_side_effects:
        return _pause(ledger, Reason.AMBIGUOUS_SIDE_EFFECTS)
    seconds_to_deadline = math.ceil((record.deadline_at - now).total_seconds())
    reservation = min(ledger.allowance_seconds, seconds_to_deadline)
    reserved = ledger.reserve(attempt=record.attempt + 1, seconds=reservation)
    return ContinuationDecision(Verdict.ALLOW, None, reserved)
