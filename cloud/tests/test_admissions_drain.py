"""Operator drain refuses new admissions and lets admitted work finish.

Drain and resume are operator actions: the owner and the scheduler are refused. Admitted runs keep
dispatching and running, and a stopping run keeps its capacity until termination is confirmed.
"""

from __future__ import annotations

from fakes import (
    CONTROLLER_HEADERS,
    OPERATOR_HEADERS,
    OWNER_A,
    admission_limits,
    create_payload,
    make_policy,
    owner_headers,
)
from flows import (
    admissions_status,
    admit_and_start,
    make_world,
    reconcile,
    to_starting,
    write_status,
)

from mao_cloud.config import Policy

DRAIN = "/internal/v1/admissions:drain"
RESUME = "/internal/v1/admissions:resume"


def _refused(response, *, code: str = "admissions_paused") -> None:
    assert response.status_code == 503, response.text
    assert response.json()["error"]["code"] == code


def test_drain_refuses_new_admissions_with_a_typed_error() -> None:
    world = make_world()

    drained = world.client.post(DRAIN, headers=OPERATOR_HEADERS)

    assert drained.status_code == 200, drained.text
    assert drained.json()["state"] == "drained"
    _refused(world.client.post("/v1/runs", json=create_payload(), headers=owner_headers()))
    assert not world.store.admission_snapshot().records


def test_drain_is_idempotent() -> None:
    world = make_world()

    first = world.client.post(DRAIN, headers=OPERATOR_HEADERS)
    second = world.client.post(DRAIN, headers=OPERATOR_HEADERS)

    assert first.status_code == 200 and second.status_code == 200
    assert second.json()["state"] == "drained"


def test_drain_lets_admitted_work_dispatch_and_keep_running() -> None:
    world = make_world()
    run_id, _, receipt = admit_and_start(world)
    world.client.post(DRAIN, headers=OPERATOR_HEADERS)

    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1, phase="running")

    assert reconcile(world, run_id)["state"] == "running"


def test_resume_reopens_admissions_on_the_next_request() -> None:
    world = make_world()
    world.client.post(DRAIN, headers=OPERATOR_HEADERS)

    resumed = world.client.post(RESUME, headers=OPERATOR_HEADERS)
    created = world.client.post("/v1/runs", json=create_payload(), headers=owner_headers())

    assert resumed.status_code == 200, resumed.text
    assert resumed.json()["state"] == "open"
    assert created.status_code == 201, created.text


def test_only_the_operator_identity_can_drain_or_resume() -> None:
    world = make_world()

    for headers in (owner_headers(OWNER_A), CONTROLLER_HEADERS):
        for path in (DRAIN, RESUME):
            refused = world.client.post(path, headers=headers)
            assert refused.status_code == 403, refused.text
            assert refused.json()["error"]["code"] == "not_allowed"

    assert world.admissions.is_open()
    assert world.client.post(
        "/v1/runs", json=create_payload(), headers=owner_headers()
    ).status_code == 201


def test_admissions_status_reports_state_policy_version_and_holding_runs() -> None:
    world = make_world(policy=_policy_with_version(7))
    assert admissions_status(world) == {"state": "open", "policy_version": 7, "holding_runs": 0}

    run_id, _, receipt = admit_and_start(world)
    to_starting(world, run_id, receipt)
    world.client.post(DRAIN, headers=OPERATOR_HEADERS)

    assert admissions_status(world) == {"state": "drained", "policy_version": 7, "holding_runs": 1}


def test_status_is_refused_to_the_owner() -> None:
    world = make_world()

    response = world.client.get("/internal/v1/admissions", headers=owner_headers(OWNER_A))

    assert response.status_code == 403, response.text
    assert response.json()["error"]["code"] == "not_allowed"


def test_drain_still_dispatches_an_already_queued_run() -> None:
    world = make_world()
    run_id, _, _ = admit_and_start(world)
    world.client.post(DRAIN, headers=OPERATOR_HEADERS)

    assert reconcile(world, run_id)["state"] == "dispatching"


def _policy_with_version(version: int) -> Policy:
    return make_policy(admission=admission_limits(version=version))
