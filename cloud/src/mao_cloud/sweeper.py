"""Sweeper planning: which runs need liveness or outstanding-capacity reconciliation.

plan_sweep only plans. The broker's reconcile applies the same liveness rules through CAS, so a
sweep that races a worker write cannot move a run it did not read.
"""

import argparse
import hashlib
import json
import sys
from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from enum import StrEnum
from pathlib import Path
from typing import Literal

from pydantic import ValidationError

from mao_cloud.config import Policy, PolicyError, RetentionDurations, load_policy
from mao_cloud.documents import InputError, parse_instant, read_json
from mao_cloud.protocol import (
    TERMINAL_STATES,
    RunRecord,
    RunState,
    TypedFailure,
    liveness_decision,
    unknown_launch_alert_due,
)

_RECONCILE_STATES = frozenset(
    {
        RunState.QUEUED,
        RunState.DISPATCHING,
        RunState.STARTING,
        RunState.RUNNING,
        RunState.COLLECTING,
        RunState.RECONCILING,
        RunState.CANCEL_REQUESTED,
    }
)


@dataclass(frozen=True)
class SweepAction:
    """One planned step for a run. reason 'liveness' carries a transition; 'reconcile' does not."""

    run_id: str
    reason: Literal["liveness", "reconcile"]
    to: RunState | None
    failure: TypedFailure | None

    def describe(self) -> dict[str, object]:
        return {
            "run_id": self.run_id,
            "reason": self.reason,
            "to": self.to.value if self.to is not None else None,
            "failure": self.failure.model_dump(mode="json") if self.failure is not None else None,
        }


RECONCILE_BACKOFF_BASE_SECONDS = 30
RECONCILE_BACKOFF_CAP_SECONDS = 900
_MAX_BACKOFF_DOUBLINGS = 16


def reconcile_backoff_seconds(attempts: int, run_id: str) -> int:
    """Seconds before the planner hints the next reconcile of an unsettled run.

    The step doubles from the base up to the cap. A deterministic jitter, derived from the run id
    and attempt count, takes off up to a quarter of the step, so runs spread out without a random
    source. The result is never above the step and never below three quarters of it.
    """
    if attempts < 1:
        raise ValueError("attempts must be at least 1")
    doublings = min(attempts - 1, _MAX_BACKOFF_DOUBLINGS)
    step = min(RECONCILE_BACKOFF_CAP_SECONDS, RECONCILE_BACKOFF_BASE_SECONDS * 2**doublings)
    digest = hashlib.sha256(f"{run_id}:{attempts}".encode()).digest()
    jitter = int.from_bytes(digest[:4], "big") % (step // 4 + 1)
    return step - jitter


@dataclass(frozen=True)
class SweepPage:
    """One bounded page of planned actions. next_after resumes the walk; None means it is complete."""

    actions: list[SweepAction]
    next_after: str | None


def plan_sweep_page(
    records: Iterable[RunRecord],
    *,
    now: datetime,
    policy: Policy,
    limit: int | None = None,
    after: str | None = None,
) -> SweepPage:
    """Plan runs after the cursor, in run-id order, until `limit` actions are planned.

    The limit bounds the actions returned, not the runs read: a run that needs no action costs
    nothing against it. A caller that does not carry the cursor therefore still reaches every due
    run within `limit` of the first one, instead of stalling on the same idle prefix each time.
    next_after is the last run of a full page while runs remain after it; None means the walk is
    complete. Planning only.
    """
    if limit is not None and limit < 1:
        raise ValueError("limit must be at least 1")
    ordered = sorted(records, key=lambda item: item.run_id)
    if after is not None:
        ordered = [record for record in ordered if record.run_id > after]
    actions: list[SweepAction] = []
    for index, record in enumerate(ordered):
        action = _plan_run(record, now=now, policy=policy)
        if action is None:
            continue
        actions.append(action)
        if limit is not None and len(actions) == limit:
            more = index + 1 < len(ordered)
            return SweepPage(actions, record.run_id if more else None)
    return SweepPage(actions, None)


def plan_sweep(
    records: Iterable[RunRecord],
    *,
    now: datetime,
    policy: Policy,
) -> list[SweepAction]:
    """Plan every run in one pass, sorted by run id. The CLI dry run uses this; bounded callers page."""
    return plan_sweep_page(records, now=now, policy=policy).actions


def _plan_run(record: RunRecord, *, now: datetime, policy: Policy) -> SweepAction | None:
    """One run's planned action. Backoff delays only reconcile hints, never a liveness decision.

    The unknown-launch alert is due once per attempt and is not delayed by backoff, so it fires on time.
    """
    alert_due = unknown_launch_alert_due(record, now=now)
    backoff_clear = record.next_reconcile_at is None or now >= record.next_reconcile_at
    if record.state in TERMINAL_STATES:
        if record.reserved_seconds > 0 and (alert_due or backoff_clear):
            return SweepAction(record.run_id, "reconcile", None, None)
        return None
    decision = liveness_decision(
        record,
        now=now,
        startup_timeout_seconds=policy.startup_timeout_seconds,
        stale_after_seconds=policy.stale_after_seconds,
        skew_seconds=policy.skew_seconds,
        queued_deadline_seconds=policy.admission.queued_deadline_seconds,
    )
    if decision is not None:
        return SweepAction(record.run_id, "liveness", decision.to, decision.failure)
    if alert_due or (record.state in _RECONCILE_STATES and backoff_clear):
        return SweepAction(record.run_id, "reconcile", None, None)
    return None


class RetentionClass(StrEnum):
    """Durable object classes with their own approved age. Each ages from the run's terminal state."""

    INPUTS = "inputs"
    RESULTS_CHECKPOINTS = "results_checkpoints"
    CLAIMS_STATUS_STATE = "claims_status_state"
    LOGS = "logs"


@dataclass(frozen=True)
class RetentionPlan:
    run_id: str
    classes: frozenset[RetentionClass]


def retention_due(
    record: RunRecord,
    *,
    now: datetime,
    retention: RetentionDurations | None,
) -> frozenset[RetentionClass]:
    """Object classes of this run whose approved age has elapsed.

    Nothing is due without approved durations, before the run is terminal, or while its capacity
    reservation is unsettled. The reservation clears only after the launch adapter confirms that no
    execution remains, so an unsettled reservation means the run may still be live.
    """
    if retention is None or record.state not in TERMINAL_STATES or record.reserved_seconds > 0:
        return frozenset()
    elapsed = now - record.state_entered_at
    ages = {
        RetentionClass.INPUTS: retention.inputs_days,
        RetentionClass.RESULTS_CHECKPOINTS: retention.results_checkpoints_days,
        RetentionClass.CLAIMS_STATUS_STATE: retention.claims_status_state_days,
        RetentionClass.LOGS: retention.logs_days,
    }
    return frozenset(cls for cls, days in ages.items() if elapsed >= timedelta(days=days))


def plan_retention(
    records: Iterable[RunRecord],
    *,
    now: datetime,
    retention: RetentionDurations | None,
) -> list[RetentionPlan]:
    """Runs with at least one due object class, sorted by run id. Planning only; nothing is deleted."""
    plan: list[RetentionPlan] = []
    for record in sorted(records, key=lambda item: item.run_id):
        classes = retention_due(record, now=now, retention=retention)
        if classes:
            plan.append(RetentionPlan(record.run_id, classes))
    return plan


def _load_records(path: Path) -> list[RunRecord]:
    payload = read_json(path)
    if not isinstance(payload, list):
        raise InputError(f"{path} must hold a JSON array of run records")
    return [RunRecord.model_validate(item) for item in payload]


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="mao-cloud-sweeper",
        description="Plan sweep actions for run records. Dry run: nothing is applied.",
    )
    parser.add_argument("--policy", required=True, type=Path, help="server policy JSON file")
    parser.add_argument("--records", required=True, type=Path, help="JSON array of run records")
    parser.add_argument("--now", type=parse_instant, help="ISO 8601 time with offset; default now")
    args = parser.parse_args(argv)
    now = args.now or datetime.now(UTC)
    try:
        policy = load_policy(args.policy)
        records = _load_records(args.records)
    except (InputError, PolicyError, ValidationError) as exc:
        print(f"mao-cloud-sweeper: {exc}", file=sys.stderr)
        return 2
    actions = plan_sweep(records, now=now, policy=policy)
    print(json.dumps([action.describe() for action in actions], indent=2, sort_keys=True))
    print("mao-cloud-sweeper: dry run; no action was applied", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
