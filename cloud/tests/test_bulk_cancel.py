"""Operator bulk cancel stops admitted runs by selection and reports a typed outcome per run.

Each run follows the owner cancel's confirmed-stop rule: a run that holds capacity stays
cancel_requested until the launcher confirms termination, and a cancelled run is never restarted
or reactivated. Bulk cancel is not gated by the drain.
"""

from __future__ import annotations

import asyncio
import threading
from typing import Any

import httpx

import pytest
from fakes import (
    CONTROLLER_HEADERS,
    OPERATOR_HEADERS,
    OWNER_A,
    owner_headers,
)
from flows import (
    admissions_status,
    admit_and_start,
    make_world,
    post_retry,
    reconcile,
    retry_body,
    to_starting,
    view,
)

BULK_CANCEL = "/internal/v1/runs:bulk-cancel"
UNKNOWN_RUN = "run_" + "f" * 32


def _bulk(world: Any, run_ids: list[str], headers: dict[str, str] = OPERATOR_HEADERS) -> Any:
    return world.client.post(
        BULK_CANCEL,
        json={"protocol_version": "v1", "run_ids": run_ids},
        headers=headers,
    )


def _outcomes(response: Any) -> list[dict[str, Any]]:
    assert response.status_code == 200, response.text
    return response.json()["outcomes"]


def test_bulk_cancel_cancels_queued_runs_and_reports_each_in_request_order() -> None:
    world = make_world()
    first, _, _ = admit_and_start(world, OWNER_A)
    second, _, _ = admit_and_start(world, OWNER_A)

    outcomes = _outcomes(_bulk(world, [second, first]))

    assert outcomes == [
        {"run_id": second, "outcome": "cancelled", "state": "cancelled", "holding_capacity": False},
        {"run_id": first, "outcome": "cancelled", "state": "cancelled", "holding_capacity": False},
    ]
    assert view(world, first)["state"] == "cancelled"
    assert view(world, second)["state"] == "cancelled"
    assert world.launcher.launched == []


def test_bulk_cancel_of_a_running_run_holds_capacity_until_stop_is_confirmed() -> None:
    world = make_world()
    run_id, _, receipt = admit_and_start(world)
    to_starting(world, run_id, receipt)

    outcomes = _outcomes(_bulk(world, [run_id]))

    assert outcomes == [
        {
            "run_id": run_id,
            "outcome": "cancel_requested",
            "state": "cancel_requested",
            "holding_capacity": True,
        }
    ]
    assert world.launcher.stop_requested(world.store.get_run(run_id))
    assert admissions_status(world)["holding_runs"] == 1

    world.launcher.confirm_stopped(world.store.get_run(run_id))
    reconcile(world, run_id)

    assert view(world, run_id)["state"] == "cancelled"
    assert admissions_status(world)["holding_runs"] == 0


def test_bulk_cancel_reports_terminal_and_unknown_runs_without_changing_them() -> None:
    world = make_world()
    run_id, _, _ = admit_and_start(world)
    _outcomes(_bulk(world, [run_id]))

    outcomes = _outcomes(_bulk(world, [run_id, UNKNOWN_RUN]))

    assert outcomes == [
        {"run_id": run_id, "outcome": "already_terminal", "state": "cancelled", "holding_capacity": False},
        {"run_id": UNKNOWN_RUN, "outcome": "not_found", "state": None, "holding_capacity": False},
    ]
    assert view(world, run_id)["state"] == "cancelled"


def test_bulk_cancelled_run_is_never_restarted_or_reactivated() -> None:
    world = make_world()
    run_id, _, _ = admit_and_start(world)
    _outcomes(_bulk(world, [run_id]))
    cancelled = view(world, run_id)

    retried = post_retry(world, run_id, retry_body(world, expected_version=cancelled["version"]))
    world.clock.advance(seconds=3600)

    assert retried.status_code == 409, retried.text
    assert _without_server_time(view(world, run_id)) == _without_server_time(cancelled)
    assert world.launcher.launched == []


def _without_server_time(run_view: dict[str, Any]) -> dict[str, Any]:
    """The run's state is what must not move; server_time reports the clock and always advances."""
    return {key: value for key, value in run_view.items() if key != "server_time"}


def test_bulk_cancel_is_not_gated_by_the_drain() -> None:
    world = make_world()
    run_id, _, _ = admit_and_start(world)
    world.client.post("/internal/v1/admissions:drain", headers=OPERATOR_HEADERS)

    outcomes = _outcomes(_bulk(world, [run_id]))

    assert outcomes[0]["outcome"] == "cancelled"


@pytest.mark.parametrize(
    "headers",
    [owner_headers(OWNER_A), CONTROLLER_HEADERS],
    ids=["owner", "scheduler"],
)
def test_only_the_operator_identity_can_bulk_cancel(headers: dict[str, str]) -> None:
    world = make_world()
    run_id, _, _ = admit_and_start(world, OWNER_A)

    refused = _bulk(world, [run_id], headers=headers)

    assert refused.status_code == 403, refused.text
    assert refused.json()["error"]["code"] == "not_allowed"
    assert view(world, run_id)["state"] == "queued"


@pytest.mark.parametrize(
    "run_ids",
    [
        pytest.param([], id="empty"),
        pytest.param([UNKNOWN_RUN, UNKNOWN_RUN], id="duplicate"),
        pytest.param([f"run_{index:032x}" for index in range(51)], id="over-limit"),
        pytest.param(["not-a-run"], id="malformed"),
    ],
)
def test_bulk_cancel_rejects_an_invalid_selection_before_changing_anything(
    run_ids: list[str],
) -> None:
    world = make_world()
    run_id, _, _ = admit_and_start(world)
    before = view(world, run_id)

    refused = _bulk(world, run_ids)

    assert refused.status_code == 400, refused.text
    assert refused.json()["error"]["code"] == "invalid_request"
    assert view(world, run_id) == before


def test_bulk_cancel_refuses_a_request_with_unknown_fields() -> None:
    world = make_world()

    response = world.client.post(
        BULK_CANCEL,
        json={"protocol_version": "v1", "run_ids": [UNKNOWN_RUN], "force": True},
        headers=OPERATOR_HEADERS,
    )

    assert response.status_code == 400, response.text
    assert response.json()["error"]["code"] == "unknown_field"


def test_bulk_cancel_does_not_block_unrelated_api_requests(monkeypatch) -> None:
    world = make_world()
    run_id, _, receipt = admit_and_start(world)
    to_starting(world, run_id, receipt)
    entered = threading.Event()
    release = threading.Event()
    original_stop = world.launcher.request_stop

    def slow_stop(record) -> None:
        entered.set()
        if not release.wait(3):
            raise RuntimeError("stop request timed out")
        original_stop(record)

    monkeypatch.setattr(world.launcher, "request_stop", slow_stop)

    async def exercise() -> None:
        async with httpx.AsyncClient(
            transport=httpx.ASGITransport(app=world.client.app), base_url="http://test"
        ) as client:
            batch = asyncio.create_task(client.post(
                BULK_CANCEL, json={"protocol_version": "v1", "run_ids": [run_id]},
                headers=OPERATOR_HEADERS,
            ))
            try:
                assert await asyncio.to_thread(entered.wait, 2)
                assert not batch.done()
                status = await asyncio.wait_for(
                    client.get("/internal/v1/admissions", headers=OPERATOR_HEADERS), timeout=1,
                )
                assert status.status_code == 200, status.text
                assert not batch.done()
            finally:
                release.set()
                response = await batch
            assert response.status_code == 200, response.text
            assert response.json()["outcomes"][0]["outcome"] == "cancel_requested"

    asyncio.run(exercise())
