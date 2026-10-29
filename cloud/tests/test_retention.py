"""Retention planning: which object classes of a run are past their approved age.

Approved durations are policy data subject to G01 approval. With no retention block in policy,
nothing is due. A run is never due before it ends or while its capacity reservation is unsettled,
because a reservation is released only after the launch adapter confirms no execution remains.
Each object class ages from the moment the run reached its terminal state.
"""

from __future__ import annotations

import json
from datetime import timedelta

import pytest
from fakes import EPOCH, policy_document, run_record

from mao_cloud.config import PolicyError, RetentionDurations, policy_from_text
from mao_cloud.protocol import RunState
from mao_cloud.sweeper import RetentionClass, plan_retention, retention_due

DURATIONS = RetentionDurations(
    inputs_days=7,
    results_checkpoints_days=14,
    claims_status_state_days=30,
    logs_days=14,
)
ALL_CLASSES = frozenset(RetentionClass)
FAR_FUTURE = EPOCH + timedelta(days=3650)


def settled(**overrides: object):
    """A terminal run whose reservation has been released, entered terminal at EPOCH."""
    return run_record(state=RunState.SUCCEEDED, reserved_seconds=0, **overrides)


def test_without_approved_durations_nothing_is_due() -> None:
    assert retention_due(settled(), now=FAR_FUTURE, retention=None) == frozenset()


@pytest.mark.parametrize(
    "state",
    [RunState.QUEUED, RunState.RUNNING, RunState.RECONCILING, RunState.CANCEL_REQUESTED],
)
def test_a_run_that_has_not_ended_is_never_due(state: RunState) -> None:
    record = run_record(state=state, reserved_seconds=0)
    assert retention_due(record, now=FAR_FUTURE, retention=DURATIONS) == frozenset()


def test_an_unsettled_reservation_holds_every_class() -> None:
    record = run_record(state=RunState.FAILED, reserved_seconds=3600)
    assert retention_due(record, now=FAR_FUTURE, retention=DURATIONS) == frozenset()


def test_each_class_becomes_due_at_its_own_age() -> None:
    record = settled()

    def due_after(days: int) -> frozenset[RetentionClass]:
        at = EPOCH + timedelta(days=days)
        return retention_due(record, now=at, retention=DURATIONS)

    assert due_after(7) == frozenset({RetentionClass.INPUTS})
    assert due_after(14) == frozenset(
        {RetentionClass.INPUTS, RetentionClass.RESULTS_CHECKPOINTS, RetentionClass.LOGS}
    )
    assert due_after(30) == ALL_CLASSES


def test_one_second_before_an_age_that_class_is_not_yet_due() -> None:
    record = settled()
    just_before = EPOCH + timedelta(days=7) - timedelta(seconds=1)
    assert retention_due(record, now=just_before, retention=DURATIONS) == frozenset()


def test_age_runs_from_when_the_run_ended_not_when_it_was_admitted() -> None:
    ended = EPOCH + timedelta(days=10)
    record = settled(state_entered_at=ended, admitted_at=EPOCH)
    before_seven_days_after_end = ended + timedelta(days=7) - timedelta(seconds=1)
    assert retention_due(record, now=before_seven_days_after_end, retention=DURATIONS) == frozenset()
    assert retention_due(
        record, now=ended + timedelta(days=7), retention=DURATIONS
    ) == frozenset({RetentionClass.INPUTS})


def test_plan_lists_only_runs_with_due_classes_in_run_id_order() -> None:
    late = settled(run_id="run_" + "b" * 32)
    early = settled(run_id="run_" + "a" * 32)
    still_queued = run_record(state=RunState.QUEUED, run_id="run_" + "c" * 32)

    plan = plan_retention(
        [late, still_queued, early], now=EPOCH + timedelta(days=14), retention=DURATIONS
    )

    assert [entry.run_id for entry in plan] == ["run_" + "a" * 32, "run_" + "b" * 32]
    assert plan[0].classes == frozenset(
        {RetentionClass.INPUTS, RetentionClass.RESULTS_CHECKPOINTS, RetentionClass.LOGS}
    )


def test_retention_durations_load_from_the_policy_file() -> None:
    document = policy_document()
    document["retention"] = {
        "inputs_days": 7,
        "results_checkpoints_days": 14,
        "claims_status_state_days": 30,
        "logs_days": 14,
    }
    assert policy_from_text(json.dumps(document)).retention == DURATIONS


def test_a_policy_without_a_retention_block_has_no_approved_durations() -> None:
    assert policy_from_text(json.dumps(policy_document())).retention is None


_BAD_RETENTION = [
    {"inputs_days": 0, "results_checkpoints_days": 14, "claims_status_state_days": 30, "logs_days": 14},
    {"inputs_days": 7, "results_checkpoints_days": 14, "claims_status_state_days": 30},
    {
        "inputs_days": 7,
        "results_checkpoints_days": 14,
        "claims_status_state_days": 30,
        "logs_days": 14,
        "archive_days": 365,
    },
]


@pytest.mark.parametrize("retention", _BAD_RETENTION)
def test_retention_rejects_non_positive_missing_or_unknown_durations(retention: dict) -> None:
    document = policy_document()
    document["retention"] = retention
    with pytest.raises(PolicyError):
        policy_from_text(json.dumps(document))
