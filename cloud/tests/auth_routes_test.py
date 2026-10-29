"""HTTP authorization: owner scope on owner routes, operator and scheduler limits, typed refusals.

Refusals are compared by their whole body, so a test pins exactly what a caller can learn from
each one: a missing run, another owner's run, and a refused credential must look alike.
"""

from collections.abc import Callable
from uuid import UUID

import pytest
from fastapi.routing import APIRoute

from auth_support import (
    CANCEL_BODY,
    NO_AUTH,
    OWNER_ROUTES,
    RETRY_BODY,
    ROGUE_SIGNER,
    UNKNOWN_RUN_ID,
    AuthWorld,
    admit_and_start_as,
    as_operator,
    as_owner_a,
    as_owner_b,
    as_scheduler,
    bearer,
    create_as,
    make_auth_world,
    mint,
    reconcile_as,
)
from fakes import OWNER_A, create_payload
from mao_cloud.protocol import RunState


@pytest.fixture
def world() -> AuthWorld:
    return make_auth_world()


def test_owner_reads_its_own_run(world: AuthWorld) -> None:
    run_id, _ = admit_and_start_as(world, as_owner_a())

    assert world.client.get(f"/v1/runs/{run_id}", headers=as_owner_a()).status_code == 200
    assert world.client.get(f"/v1/runs/{run_id}/status", headers=as_owner_a()).status_code == 200
    assert world.client.get(f"/v1/runs/{run_id}/logs", headers=as_owner_a()).status_code == 200
    result = world.client.get(f"/v1/runs/{run_id}/result", headers=as_owner_a())
    assert result.status_code == 409
    assert result.json()["error"]["code"] == "result_not_ready"


def test_owner_may_cancel_its_own_run(world: AuthWorld) -> None:
    run_id, _ = admit_and_start_as(world, as_owner_a())

    cancelled = world.client.post(
        f"/v1/runs/{run_id}:cancel", json=CANCEL_BODY, headers=as_owner_a()
    )

    assert cancelled.status_code == 200, cancelled.text


def test_owner_clears_the_gate_on_retry(world: AuthWorld) -> None:
    # A queued run is not a retry candidate, so the retry itself is refused by the state machine.
    # What this pins is that the owner's identity passes the authorization gate: a 403 or 404
    # here would mean the owner was refused before the state machine ran.
    run_id, _ = admit_and_start_as(world, as_owner_a())

    retried = world.client.post(f"/v1/runs/{run_id}:retry", json=RETRY_BODY, headers=as_owner_a())

    assert retried.status_code not in (403, 404), retried.text


@pytest.mark.parametrize("route", OWNER_ROUTES)
def test_other_owner_gets_the_refusal_for_a_run_that_does_not_exist(
    world: AuthWorld, route: str
) -> None:
    run_id, _ = create_as(world, as_owner_a())
    before = world.store.get_run(run_id)
    call = OWNER_ROUTES[route]

    cross_owner = call(world.client, run_id, as_owner_b())
    missing = call(world.client, UNKNOWN_RUN_ID, as_owner_b())

    assert cross_owner.status_code == 404
    assert cross_owner.status_code == missing.status_code
    assert cross_owner.json() == missing.json()
    assert world.store.get_run(run_id) == before


@pytest.mark.parametrize("route", OWNER_ROUTES)
def test_unauthenticated_and_invalid_callers_get_one_identical_refusal(
    world: AuthWorld, route: str
) -> None:
    run_id, _ = create_as(world, as_owner_a())
    call = OWNER_ROUTES[route]

    missing = call(world.client, run_id, NO_AUTH)
    forged = call(world.client, run_id, bearer(mint(ROGUE_SIGNER)))
    unknown_run = call(world.client, UNKNOWN_RUN_ID, NO_AUTH)

    assert missing.status_code == 403
    assert forged.status_code == 403
    assert forged.json() == missing.json()
    assert unknown_run.status_code == 403
    assert unknown_run.json() == missing.json()


@pytest.mark.parametrize("route", OWNER_ROUTES)
@pytest.mark.parametrize("caller", [as_operator, as_scheduler], ids=["operator", "scheduler"])
def test_operator_and_scheduler_are_refused_on_every_owner_route(
    world: AuthWorld, route: str, caller: Callable[[], dict[str, str]]
) -> None:
    run_id, _ = create_as(world, as_owner_a())
    before = world.store.get_run(run_id)
    call = OWNER_ROUTES[route]

    refused = call(world.client, run_id, caller())

    assert refused.status_code == 403
    assert refused.json() == call(world.client, run_id, NO_AUTH).json()
    assert world.store.get_run(run_id) == before


def test_owner_creates_a_run_bound_to_its_own_owner_id(world: AuthWorld) -> None:
    run_id, create = create_as(world, as_owner_a())

    assert world.store.get_run(run_id).owner_id == OWNER_A
    assert world.store.find_run_by_request(OWNER_A, UUID(create["request_uuid"])) is not None


@pytest.mark.parametrize(
    "caller",
    [lambda: NO_AUTH, as_operator, as_scheduler],
    ids=["anonymous", "operator", "scheduler"],
)
def test_only_an_owner_creates_runs(world: AuthWorld, caller: Callable[[], dict[str, str]]) -> None:
    response = world.client.post("/v1/runs", json=create_payload(), headers=caller())

    assert response.status_code == 403


@pytest.mark.parametrize("field", ["owner", "owner_id"])
def test_owner_named_in_the_create_body_is_refused_and_creates_nothing(
    world: AuthWorld, field: str
) -> None:
    # Added after the digest is computed: the screen must refuse the field, not the digest.
    payload = {**create_payload(), field: OWNER_A}

    response = world.client.post("/v1/runs", json=payload, headers=as_owner_a())

    assert response.status_code == 400
    assert response.json()["error"]["code"] == "client_controlled_field"
    assert world.store.find_run_by_request(OWNER_A, UUID(payload["request_uuid"])) is None


def test_reconcile_is_refused_for_an_owner(world: AuthWorld) -> None:
    run_id, _ = admit_and_start_as(world, as_owner_a())
    before = world.store.get_run(run_id)

    refused = reconcile_as(world, run_id, as_owner_a())

    assert refused.status_code == 403
    assert refused.json()["error"]["code"] == "not_allowed"
    assert world.store.get_run(run_id) == before


def test_reconcile_is_refused_without_a_caller(world: AuthWorld) -> None:
    run_id, _ = admit_and_start_as(world, as_owner_a())

    assert reconcile_as(world, run_id, NO_AUTH).status_code == 403


def test_scheduler_reconciles_any_run(world: AuthWorld) -> None:
    run_id, _ = admit_and_start_as(world, as_owner_a())

    response = reconcile_as(world, run_id, as_scheduler())

    assert response.status_code == 200, response.text
    assert response.json()["state"] == RunState.DISPATCHING.value
    assert len(world.launcher.launched) == 1


def test_operator_may_reconcile_but_not_own_a_run(world: AuthWorld) -> None:
    run_id, _ = admit_and_start_as(world, as_owner_a())

    assert reconcile_as(world, run_id, as_operator()).status_code == 200
    assert world.client.get(f"/v1/runs/{run_id}", headers=as_operator()).status_code == 403


def test_scheduler_reconcile_of_an_unknown_run_is_not_found(world: AuthWorld) -> None:
    response = reconcile_as(world, UNKNOWN_RUN_ID, as_scheduler())

    assert response.status_code == 404
    assert response.json()["error"]["code"] == "not_found"


def test_owner_header_from_another_owner_cannot_reach_a_run(world: AuthWorld) -> None:
    run_id, _ = create_as(world, as_owner_a())
    spoofed = {**as_owner_b(), "x-owner": OWNER_A}
    missing = world.client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=as_owner_b())

    response = world.client.get(f"/v1/runs/{run_id}", headers=spoofed)

    assert response.status_code == 404
    assert response.json() == missing.json()


def test_owner_header_without_a_credential_grants_nothing(world: AuthWorld) -> None:
    run_id, _ = create_as(world, as_owner_a())

    response = world.client.get(f"/v1/runs/{run_id}", headers={"x-owner": OWNER_A})

    assert response.status_code == 403


def test_owner_header_on_a_scheduler_credential_grants_nothing(world: AuthWorld) -> None:
    run_id, _ = create_as(world, as_owner_a())

    headers = {**as_scheduler(), "x-owner": OWNER_A}

    response = world.client.get(f"/v1/runs/{run_id}", headers=headers)

    assert response.status_code == 403


def test_api_exposes_only_the_authenticated_protocol_routes(world: AuthWorld) -> None:
    exposed = {
        (route.path, frozenset(route.methods) - {"HEAD"})
        for route in world.client.app.routes
        if isinstance(route, APIRoute)
    }

    assert exposed == {
        ("/v1/runs", frozenset({"POST"})),
        ("/v1/runs/{run_id}", frozenset({"GET"})),
        ("/v1/runs/{run_id}:start", frozenset({"POST"})),
        ("/v1/runs/{run_id}:cancel", frozenset({"POST"})),
        ("/v1/runs/{run_id}:retry", frozenset({"POST"})),
        ("/v1/runs/{run_id}/status", frozenset({"GET"})),
        ("/v1/runs/{run_id}/teardown", frozenset({"GET"})),
        ("/v1/runs/{run_id}/logs", frozenset({"GET"})),
        ("/v1/runs/{run_id}/result", frozenset({"GET"})),
        ("/internal/v1/runs/{run_id}:reconcile", frozenset({"POST"})),
        ("/internal/v1/admissions", frozenset({"GET"})),
        ("/internal/v1/admissions:drain", frozenset({"POST"})),
        ("/internal/v1/admissions:resume", frozenset({"POST"})),
        ("/internal/v1/runs:bulk-cancel", frozenset({"POST"})),
        ("/internal/v1/runs:reconcile", frozenset({"POST"})),
    }
