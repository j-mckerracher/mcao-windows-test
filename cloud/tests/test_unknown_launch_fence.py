"""Unknown launches stay fenced (DPES-809 AC3, AC5).

A dispatch whose execution has never reported keeps its capacity reservation. Neither the
ten-minute alert, the stale window, nor the deadline authorises a second launch or a retry. Only
the launcher's confirmed absence or termination releases the reservation.
"""

from typing import Any

from fakes import OWNER_A, owner_headers, uuid7
from flows import World, admit_and_start, dispatch, reconcile
from mao_cloud.protocol import TERMINAL_STATES

LONG_BUDGET = {"max_runtime_seconds": 3600}


def _retry(world: World, run_id: str, *, expected_version: int, request_uuid: Any = None) -> Any:
    body = {
        "protocol_version": "v1",
        "request_uuid": str(request_uuid or uuid7(world.clock.now())),
        "expected_version": expected_version,
        "expected_attempt": 1,
    }
    return world.client.post(
        f"/v1/runs/{run_id}:retry", json=body, headers=owner_headers(OWNER_A)
    )


def _unconfirmed_launch_uncertain(world: World) -> str:
    """Dispatch, go silent past startup and the stale window, and fail. Nothing is confirmed stopped."""
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    dispatch(world, run_id)
    world.clock.advance(seconds=601)
    assert reconcile(world, run_id)["state"] == "reconciling"
    world.clock.advance(seconds=121)
    view = reconcile(world, run_id)
    assert view["state"] == "failed"
    assert view["failure"]["code"] == "launch_uncertain"
    return run_id


def test_unknown_launch_alerts_once_at_ten_minutes_and_keeps_capacity(world: World) -> None:
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    dispatch(world, run_id)

    world.clock.advance(seconds=599)
    reconcile(world, run_id)
    assert world.store.get_run(run_id).unknown_launch_alerted_at is None

    world.clock.advance(seconds=2)
    reconcile(world, run_id)
    first = world.store.get_run(run_id)
    assert first.unknown_launch_alerted_at is not None
    assert first.reserved_seconds > 0

    reconcile(world, run_id)
    world.clock.advance(seconds=60)
    reconcile(world, run_id)
    later = world.store.get_run(run_id)
    assert later.unknown_launch_alerted_at == first.unknown_launch_alerted_at
    assert later.reserved_seconds == first.reserved_seconds > 0
    assert world.launcher.calls == 1


def test_retry_is_refused_while_an_unconfirmed_launch_holds_capacity(world: World) -> None:
    run_id = _unconfirmed_launch_uncertain(world)
    before = world.store.get_run(run_id)
    assert before.reserved_seconds > 0

    response = _retry(world, run_id, expected_version=before.version)
    assert response.status_code == 409, response.text
    assert "retry_not_allowed" in response.text

    reconcile(world, run_id)
    reconcile(world, run_id)
    after = world.store.get_run(run_id)
    assert after.attempt == 1
    assert after.reserved_seconds == before.reserved_seconds
    assert world.launcher.calls == 1


def test_stale_retry_version_is_refused_while_capacity_is_held(world: World) -> None:
    run_id = _unconfirmed_launch_uncertain(world)
    before = world.store.get_run(run_id)

    response = _retry(world, run_id, expected_version=before.version - 1)
    assert response.status_code == 409, response.text
    assert world.store.get_run(run_id).attempt == 1
    assert world.launcher.calls == 1


def test_confirmed_absence_releases_capacity_and_allows_exactly_one_relaunch(world: World) -> None:
    run_id = _unconfirmed_launch_uncertain(world)
    world.launcher.confirm_stopped(world.store.get_run(run_id))
    reconcile(world, run_id)
    released = world.store.get_run(run_id)
    assert released.reserved_seconds == 0
    assert released.unknown_launch_alerted_at is not None

    response = _retry(world, run_id, expected_version=released.version)
    assert response.status_code == 200, response.text
    assert world.store.get_run(run_id).unknown_launch_alerted_at is None
    assert reconcile(world, run_id)["state"] == "dispatching"
    reconcile(world, run_id)
    reconcile(world, run_id)

    assert world.launcher.calls == 2
    assert world.store.get_run(run_id).attempt == 2


def test_deadline_during_an_unconfirmed_launch_keeps_capacity_until_stop_is_confirmed(
    world: World,
) -> None:
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    dispatch(world, run_id)

    world.clock.advance(seconds=3700)
    assert reconcile(world, run_id)["state"] == "cancel_requested"
    held = world.store.get_run(run_id)
    assert held.reserved_seconds > 0
    assert held.unknown_launch_alerted_at is not None

    reconcile(world, run_id)
    reconcile(world, run_id)
    assert world.store.get_run(run_id).reserved_seconds == held.reserved_seconds
    assert world.launcher.calls == 1
    assert world.launcher.stop_requests

    world.launcher.confirm_stopped(world.store.get_run(run_id))
    reconcile(world, run_id)
    settled = world.store.get_run(run_id)
    assert settled.state in TERMINAL_STATES
    assert settled.reserved_seconds == 0
    assert world.launcher.calls == 1
