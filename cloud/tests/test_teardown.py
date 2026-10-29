"""Teardown outcomes: classify_cleanup precedence and the pending-deletion set.

The outcome is a pure function of the observed facts, so these tests enumerate the fact space
rather than sampling a few cases.
"""

from itertools import product

import pytest

from fakes import run_record
from mao_cloud.protocol import (
    PENDING_DELETION_OUTCOMES,
    CleanupFacts,
    CleanupOutcome,
    classify_cleanup,
    teardown_status,
)


def _facts(**overrides: bool) -> CleanupFacts:
    base = {
        "store_reachable": True,
        "retention_exception": False,
        "execution_terminal": True,
        "capability_expiry_passed": True,
        "data_present": True,
        "deletion_started": False,
        "records_past_retention": False,
    }
    base.update(overrides)
    return CleanupFacts(**base)


def test_an_unreachable_store_is_unavailable_whatever_else_is_true() -> None:
    for combo in product([True, False], repeat=6):
        facts = _facts(
            store_reachable=False,
            retention_exception=combo[0],
            execution_terminal=combo[1],
            capability_expiry_passed=combo[2],
            data_present=combo[3],
            deletion_started=combo[4],
            records_past_retention=combo[5],
        )
        assert classify_cleanup(facts) is CleanupOutcome.UNAVAILABLE


def test_a_retention_exception_outranks_a_running_run() -> None:
    facts = _facts(retention_exception=True, execution_terminal=False)
    assert classify_cleanup(facts) is CleanupOutcome.RETAINED_EXCEPTION


def test_a_live_run_is_running_before_any_deletion_rule_applies() -> None:
    facts = _facts(execution_terminal=False, capability_expiry_passed=False, data_present=False)
    assert classify_cleanup(facts) is CleanupOutcome.RUNNING


def test_an_unexpired_capability_fences_a_terminal_run() -> None:
    facts = _facts(capability_expiry_passed=False, deletion_started=True)
    assert classify_cleanup(facts) is CleanupOutcome.FENCED


def test_absent_data_past_retention_is_expired() -> None:
    facts = _facts(data_present=False, records_past_retention=True)
    assert classify_cleanup(facts) is CleanupOutcome.EXPIRED


def test_absent_data_within_retention_is_complete() -> None:
    facts = _facts(data_present=False, records_past_retention=False)
    assert classify_cleanup(facts) is CleanupOutcome.COMPLETE


def test_present_data_with_deletion_begun_is_incomplete() -> None:
    assert classify_cleanup(_facts(deletion_started=True)) is CleanupOutcome.INCOMPLETE


def test_present_data_not_yet_deleted_is_eligible() -> None:
    assert classify_cleanup(_facts(deletion_started=False)) is CleanupOutcome.ELIGIBLE


def test_every_fact_combination_maps_to_exactly_one_outcome() -> None:
    names = [
        "store_reachable",
        "retention_exception",
        "execution_terminal",
        "capability_expiry_passed",
        "data_present",
        "deletion_started",
        "records_past_retention",
    ]
    for combo in product([True, False], repeat=len(names)):
        outcome = classify_cleanup(_facts(**dict(zip(names, combo, strict=True))))
        assert isinstance(outcome, CleanupOutcome)


def test_the_pending_deletion_set_is_fenced_eligible_and_incomplete() -> None:
    assert PENDING_DELETION_OUTCOMES == {
        CleanupOutcome.FENCED,
        CleanupOutcome.ELIGIBLE,
        CleanupOutcome.INCOMPLETE,
    }


_OUTCOME_FACTS: dict[CleanupOutcome, dict[str, bool]] = {
    CleanupOutcome.UNAVAILABLE: {"store_reachable": False},
    CleanupOutcome.RETAINED_EXCEPTION: {"retention_exception": True},
    CleanupOutcome.RUNNING: {"execution_terminal": False},
    CleanupOutcome.FENCED: {"capability_expiry_passed": False},
    CleanupOutcome.EXPIRED: {"data_present": False, "records_past_retention": True},
    CleanupOutcome.COMPLETE: {"data_present": False},
    CleanupOutcome.INCOMPLETE: {"deletion_started": True},
    CleanupOutcome.ELIGIBLE: {},
}


def test_the_outcome_table_names_every_outcome() -> None:
    assert set(_OUTCOME_FACTS) == set(CleanupOutcome)


@pytest.mark.parametrize("outcome", list(CleanupOutcome))
def test_teardown_status_marks_pending_deletion_exactly_for_the_set(outcome: CleanupOutcome) -> None:
    facts = _facts(**_OUTCOME_FACTS[outcome])
    record = run_record()
    status = teardown_status(record, facts)
    assert classify_cleanup(facts) is outcome
    assert status.data_cleanup is outcome
    assert status.run_id == record.run_id
    assert status.execution_state == record.state
    assert status.pending_deletion is (outcome in PENDING_DELETION_OUTCOMES)


def test_teardown_status_keeps_the_request_and_audit_records_and_implies_no_fresh_run() -> None:
    status = teardown_status(run_record(), _facts())
    assert status.preserved_records == ("idempotency_record", "audit_record")
    assert status.fresh_run_implied is False
