"""Admission limits come from the policy document, not from code.

Changing a limit in the document changes the broker's answer with no code change. A missing,
malformed, or out-of-range admission section refuses to load with a PolicyError.
"""

from __future__ import annotations

import json
from collections.abc import Callable
from datetime import timedelta
from typing import Any

import pytest
from fakes import (
    EPOCH,
    OWNER_A,
    OWNER_B,
    SOURCE_TAR,
    admission_limits,
    create_payload,
    owner_headers,
    policy_document,
    run_record,
    start_payload,
    uuid7,
)
from flows import admit_and_start, dispatch, make_world, reconcile

from mao_cloud.admission import select_next_dispatch
from mao_cloud.config import PolicyError, policy_from_text
from mao_cloud.protocol import FailureCode, RunState, liveness_decision


BUDGET = {"max_runtime_seconds": 600}


def _document(**admission: Any) -> str:
    document = policy_document()
    document["admission"] = {**document["admission"], **admission}
    return json.dumps(document)


def _admit(world: Any, owner: str, index: int) -> Any:
    """Create and start one run. Returns the start response, which carries any refusal."""
    create = create_payload(request_uuid=uuid7(EPOCH + timedelta(seconds=index)))
    created = world.client.post("/v1/runs", json=create, headers=owner_headers(owner))
    assert created.status_code == 201, created.text
    run_id = created.json()["run"]["run_id"]
    name = created.json()["upload"]["object_name"]
    world.objects.write(name, SOURCE_TAR)
    return world.client.post(
        f"/v1/runs/{run_id}:start",
        json=start_payload(create, generation=world.objects.generation_of(name)),
        headers=owner_headers(owner),
    )


@pytest.mark.parametrize("owner_queue_depth", [1, 2])
def test_owner_queue_depth_follows_the_policy_document(owner_queue_depth: int) -> None:
    world = make_world(policy=policy_from_text(_document(owner_queue_depth=owner_queue_depth)))

    for index in range(owner_queue_depth):
        assert _admit(world, OWNER_A, index).status_code == 200

    refused = _admit(world, OWNER_A, owner_queue_depth)
    assert refused.status_code == 429, refused.text
    assert refused.json()["error"]["code"] == "queue_full"
    assert refused.json()["error"]["detail"]["limit"] == owner_queue_depth


@pytest.mark.parametrize("team_queue_depth", [1, 2])
def test_team_queue_depth_follows_the_policy_document(team_queue_depth: int) -> None:
    world = make_world(policy=policy_from_text(_document(team_queue_depth=team_queue_depth)))

    for index in range(team_queue_depth):
        assert _admit(world, OWNER_A if index % 2 == 0 else OWNER_B, index).status_code == 200

    refused = _admit(world, OWNER_B, team_queue_depth)
    assert refused.status_code == 429, refused.text
    assert refused.json()["error"]["code"] == "queue_full"
    assert refused.json()["error"]["detail"]["scope"] == "team"


def _admit_pair(world: Any) -> tuple[str, str]:
    """Admit OWNER_A then OWNER_B one second apart; the earlier admission dispatches first."""
    first, _, _ = admit_and_start(world, OWNER_A, budget=BUDGET)
    world.clock.advance(seconds=1)
    second, _, _ = admit_and_start(world, OWNER_B, budget=BUDGET)
    return first, second


def test_team_worker_slots_follow_the_policy_document() -> None:
    world = make_world(policy=policy_from_text(_document(team_worker_slots=1)))
    first, second = _admit_pair(world)

    dispatch(world, first)

    assert reconcile(world, second)["state"] == "queued"


def test_raising_the_team_worker_slots_in_the_policy_dispatches_the_held_run() -> None:
    world = make_world(policy=policy_from_text(_document(team_worker_slots=2)))
    first, second = _admit_pair(world)

    dispatch(world, first)
    dispatch(world, second)


def test_daily_worker_seconds_cap_follows_the_policy_document() -> None:
    # Each run reserves its 600-second budget at dispatch, so a 600-second day holds the second run.
    capped = make_world(policy=policy_from_text(_document(team_daily_worker_seconds=600)))
    first, second = _admit_pair(capped)
    dispatch(capped, first)
    assert reconcile(capped, second)["state"] == "queued"

    roomy = make_world(policy=policy_from_text(_document(team_daily_worker_seconds=1200)))
    first, second = _admit_pair(roomy)
    dispatch(roomy, first)
    dispatch(roomy, second)


def test_queued_deadline_follows_the_policy_document() -> None:
    queued = run_record(state=RunState.QUEUED, state_entered_at=EPOCH)
    later = EPOCH + timedelta(seconds=120)

    assert select_next_dispatch(
        [queued], now=later, limits=admission_limits(queued_deadline_seconds=60)
    ) is None
    assert (
        select_next_dispatch([queued], now=later, limits=admission_limits())
        == queued
    )


_INVALID_ADMISSION: list[tuple[str, Callable[[dict[str, Any]], None]]] = [
    ("missing-section", lambda document: document.pop("admission")),
    ("missing-field", lambda document: document["admission"].pop("owner_queue_depth")),
    ("unversioned", lambda document: document["admission"].pop("version")),
    ("zero-version", lambda document: document["admission"].update(version=0)),
    ("zero-depth", lambda document: document["admission"].update(owner_queue_depth=0)),
    ("negative-cap", lambda document: document["admission"].update(team_daily_worker_seconds=-1)),
    ("wrong-type", lambda document: document["admission"].update(queued_deadline_seconds="soon")),
    ("unknown-field", lambda document: document["admission"].update(team_worker_slot=4)),
]


@pytest.mark.parametrize(
    ("mutate",),
    [pytest.param(mutate, id=name) for name, mutate in _INVALID_ADMISSION],
)
def test_an_invalid_or_missing_admission_policy_fails_closed(
    mutate: Callable[[dict[str, Any]], None],
) -> None:
    document = policy_document()
    mutate(document)

    with pytest.raises(PolicyError):
        policy_from_text(json.dumps(document))


_ADMISSION_FIELDS = (
    "version", "team_queue_depth", "owner_queue_depth", "team_worker_slots",
    "owner_worker_slots", "team_daily_worker_seconds", "queued_deadline_seconds",
)


@pytest.mark.parametrize("name", _ADMISSION_FIELDS)
@pytest.mark.parametrize("value", [True, False, "600", 600.0, 1.5])
def test_admission_document_rejects_coercible_noninteger_values(name: str, value: Any) -> None:
    with pytest.raises(PolicyError):
        policy_from_text(_document(**{name: value}))


@pytest.mark.parametrize("name", _ADMISSION_FIELDS)
@pytest.mark.parametrize("value", [True, False, "600", 600.0, 1.5])
def test_direct_admission_limits_reject_noninteger_values(name: str, value: Any) -> None:
    with pytest.raises(PolicyError):
        admission_limits(**{name: value})


@pytest.mark.parametrize("from_document", [True, False])
def test_large_queue_duration_keeps_dispatch_and_liveness_bounded_by_run_deadline(
    from_document: bool,
) -> None:
    seconds = 10**15
    limits = (
        policy_from_text(_document(queued_deadline_seconds=seconds)).admission
        if from_document else admission_limits(queued_deadline_seconds=seconds)
    )
    queued = run_record(
        state=RunState.QUEUED, state_entered_at=EPOCH,
        deadline_at=EPOCH + timedelta(seconds=600),
    )
    before = EPOCH + timedelta(seconds=599)
    deadline = EPOCH + timedelta(seconds=600)

    assert select_next_dispatch([queued], now=before, limits=limits) == queued
    assert select_next_dispatch([queued], now=deadline, limits=limits) is None
    assert liveness_decision(
        queued, now=before, startup_timeout_seconds=600, stale_after_seconds=120,
        skew_seconds=30, queued_deadline_seconds=limits.queued_deadline_seconds,
    ) is None
    decision = liveness_decision(
        queued, now=deadline, startup_timeout_seconds=600, stale_after_seconds=120,
        skew_seconds=30, queued_deadline_seconds=limits.queued_deadline_seconds,
    )
    assert decision is not None
    assert decision.to is RunState.TIMED_OUT
    assert decision.failure is not None
    assert decision.failure.code is FailureCode.QUEUED_EXPIRED
