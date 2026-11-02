"""Authoritative time at the HTTP boundary: client stamps, worker timestamps, and the deadline.

The server clock decides admission, liveness, and the deadline. A client or worker timestamp may
sit ahead of or behind it, but neither may extend an allowance or create liveness. The unit-level
rules live in test_time.py; these tests show the same rules through the API.
"""

from datetime import timedelta

from fakes import EPOCH, create_payload, owner_headers
from flows import (
    World,
    admit_and_start,
    reconcile,
    to_starting,
    view,
    write_status,
)
from mao_cloud.protocol import RunView

LONG_BUDGET = {"max_runtime_seconds": 3600}


# -- client-stamped request UUIDs ---------------------------------------------


def test_request_stamped_twenty_seconds_ahead_is_admitted_on_server_time(world: World) -> None:
    create = create_payload(at=EPOCH + timedelta(seconds=20))
    response = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert response.status_code == 201, response.text

    run = RunView.model_validate(response.json()["run"])
    assert run.server_time == EPOCH
    # The deadline is the server's admission instant plus the budget. The client's stamp of
    # EPOCH + 20 s must not move it.
    assert run.deadline_at == EPOCH + timedelta(seconds=600)


def test_request_stamped_thirty_one_seconds_ahead_is_refused(world: World) -> None:
    create = create_payload(at=EPOCH + timedelta(seconds=31))
    response = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert response.status_code == 400, response.text
    assert response.json()["error"]["code"] == "invalid_request"
    assert world.store.run_count() == 0


# -- worker timestamps ahead of the server clock ------------------------------


def test_worker_timestamp_far_ahead_does_not_extend_staleness(world: World) -> None:
    run_id, _, receipt = admit_and_start(world, budget=LONG_BUDGET)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1, observed_at=EPOCH + timedelta(hours=1))
    assert reconcile(world, run_id)["state"] == "running"

    # Staleness is measured from the server's receipt of the status, not from the worker's stamp.
    world.clock.advance(seconds=121)
    assert reconcile(world, run_id)["state"] == "reconciling"


def test_worker_timestamp_far_ahead_does_not_extend_the_deadline(world: World) -> None:
    run_id, _, receipt = admit_and_start(world, budget=LONG_BUDGET)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1, observed_at=EPOCH + timedelta(hours=2))
    assert reconcile(world, run_id)["state"] == "running"

    world.clock.advance(seconds=3600)
    final = reconcile(world, run_id)
    assert final["state"] == "cancel_requested"
    assert view(world, run_id)["state"] == "cancel_requested"


# -- worker timestamps behind the last observed one ---------------------------


def test_worker_timestamp_behind_the_last_observed_does_not_regress_liveness(
    world: World,
) -> None:
    run_id, _, receipt = admit_and_start(world, budget=LONG_BUDGET)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1)
    assert reconcile(world, run_id)["state"] == "running"

    world.clock.advance(seconds=100)
    write_status(world, bootstrap, seq=2, observed_at=EPOCH - timedelta(minutes=10))
    updated = reconcile(world, run_id)
    assert updated["state"] == "running"
    assert RunView.model_validate(updated).last_seen_at == world.clock.now()

    # 121 s after the server received the backward-stamped status. A regressed last_seen,
    # taken from the worker's stamp, would already have gone stale at the reconcile above.
    world.clock.advance(seconds=121)
    assert reconcile(world, run_id)["state"] == "reconciling"
