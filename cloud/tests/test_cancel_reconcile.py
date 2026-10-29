"""DPES-816: owner cancel waits for confirmed termination, requests a platform stop, and
quarantines late results.

Everything runs against the fakes: no cloud, no platform cancellation, no real worker.
"""

from typing import Any

from fakes import make_policy, owner_headers
from flows import (
    LONG_BUDGET,
    World,
    admit_and_start,
    dispatch,
    drive_to_launch_uncertain,
    post_retry,
    reconcile,
    retry_body,
    to_starting,
    write_status,
)
from mao_cloud.protocol import (
    ArtifactRef,
    CancelDocument,
    ObjectKind,
    RunState,
    artifact_object_name,
    object_name,
    sha256_hex,
)
from mao_cloud.runner import plan_result
from mao_cloud.sweeper import plan_sweep

CANCEL_BODY = {"protocol_version": "v1"}
PAST_MAX_RUNTIME_SECONDS = 3 * 3600


def _cancel(world: World, run_id: str) -> dict[str, Any]:
    response = world.client.post(
        f"/v1/runs/{run_id}:cancel", json=CANCEL_BODY, headers=owner_headers()
    )
    assert response.status_code == 200, response.text
    return response.json()


def _running(world: World) -> tuple[str, Any, Any]:
    run_id, _, receipt = admit_and_start(world, budget=LONG_BUDGET)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1)
    assert reconcile(world, run_id)["state"] == "running"
    return run_id, bootstrap, receipt


# -- queued work never launches -----------------------------------------------


def test_cancel_of_a_queued_run_never_launches_or_requests_a_stop(world: World) -> None:
    run_id, _, _ = admit_and_start(world)
    assert _cancel(world, run_id)["state"] == "cancelled"

    assert reconcile(world, run_id)["state"] == "cancelled"
    assert world.launcher.launched == []
    assert world.launcher.stop_requests == []


# -- platform stop request ----------------------------------------------------


def test_owner_cancel_requests_a_platform_stop_and_retries_a_failed_request(world: World) -> None:
    run_id, _, _ = _running(world)
    world.launcher.fail_stop_next = True

    assert _cancel(world, run_id)["state"] == "cancel_requested"
    record = world.store.get_run(run_id)
    assert not world.launcher.stop_requested(record)

    assert reconcile(world, run_id)["state"] == "cancel_requested"
    assert world.launcher.stop_requested(record)


def test_cancel_of_a_dispatching_run_requests_a_stop_before_the_worker_claims_it(
    world: World,
) -> None:
    run_id, _, _ = admit_and_start(world)
    dispatch(world, run_id)

    assert _cancel(world, run_id)["state"] == "cancel_requested"
    assert world.launcher.stop_requested(world.store.get_run(run_id))


# -- attempt cancel marker: monotonic, create-only, retried -------------------


def _marker_name(world: World, run_id: str) -> str:
    attempt = world.store.get_run(run_id).attempt
    return object_name(ObjectKind.CANCEL, run_id=run_id, attempt=attempt)


def test_owner_cancel_writes_a_cancel_marker_that_is_never_rewritten(world: World) -> None:
    run_id, _, _ = _running(world)
    _cancel(world, run_id)
    record = world.store.get_run(run_id)

    written = world.objects.get(_marker_name(world, run_id))
    assert written is not None
    marker = CancelDocument.model_validate_json(written)
    assert (marker.run_id, marker.attempt, marker.fence, marker.claim_id) == (
        run_id,
        record.attempt,
        record.fence,
        record.claim_id,
    )

    reconcile(world, run_id)
    assert world.objects.get(_marker_name(world, run_id)) == written


def test_failed_cancel_marker_write_is_retried_on_reconcile(world: World) -> None:
    run_id, _, _ = _running(world)
    real_create_only = world.objects.create_only
    failures_left = [1]

    def flaky_create_only(name: str, data: bytes) -> None:
        if name.endswith("/cancel.json") and failures_left[0]:
            failures_left[0] -= 1
            raise RuntimeError("object store unavailable")
        real_create_only(name, data)

    world.objects.create_only = flaky_create_only  # type: ignore[method-assign]

    # The marker write fails, but the cancel is still accepted and stays open.
    assert _cancel(world, run_id)["state"] == "cancel_requested"
    assert world.objects.get(_marker_name(world, run_id)) is None
    assert world.launcher.stop_requested(world.store.get_run(run_id))

    assert reconcile(world, run_id)["state"] == "cancel_requested"
    assert world.objects.get(_marker_name(world, run_id)) is not None


# -- termination must be confirmed before the owner cancel completes ----------


def test_owner_cancel_past_the_task_deadline_waits_for_confirmed_stop(world: World) -> None:
    run_id, _, _ = _running(world)
    assert _cancel(world, run_id)["state"] == "cancel_requested"

    world.clock.advance(seconds=PAST_MAX_RUNTIME_SECONDS)
    assert reconcile(world, run_id)["state"] == "cancel_requested"

    world.launcher.confirm_stopped(world.store.get_run(run_id))
    final = reconcile(world, run_id)
    assert final["state"] == "cancelled"
    assert final["failure"]["code"] == "owner_cancelled"


# -- late results after cancel are quarantined --------------------------------


def test_late_result_after_owner_cancel_is_quarantined_and_never_succeeds(world: World) -> None:
    run_id, bootstrap, _ = _running(world)
    _cancel(world, run_id)

    report = b"done\n"
    world.objects.write(artifact_object_name(run_id, 1, "report.txt"), report)
    artifact = ArtifactRef(name="report.txt", sha256=sha256_hex(report), size_bytes=len(report))
    world.objects.put_planned(
        plan_result(
            bootstrap,
            outcome="succeeded",
            exit_code=0,
            commit_sha="b" * 40,
            artifacts=[artifact],
        )
    )
    assert reconcile(world, run_id)["state"] == "cancel_requested"

    world.launcher.confirm_stopped(world.store.get_run(run_id))
    final = reconcile(world, run_id)
    assert final["state"] == "cancelled"
    assert final["failure"]["code"] == "owner_cancelled"
    assert reconcile(world, run_id)["state"] == "cancelled"


# -- explicit retry: a new claim only after the prior execution is confirmed gone -----------


def test_retry_after_a_confirmed_stop_dispatches_under_a_new_claim(world: World) -> None:
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    drive_to_launch_uncertain(world, run_id)
    settled = world.store.get_run(run_id)
    assert settled.state is RunState.FAILED
    assert settled.reserved_seconds == 0

    response = post_retry(world, run_id, retry_body(world, expected_version=settled.version))
    assert response.status_code == 200, response.text
    assert world.store.get_run(run_id).claim_id is None

    dispatch(world, run_id)
    retried = world.store.get_run(run_id)
    assert (retried.attempt, retried.fence) == (2, 2)
    assert retried.claim_id not in (None, settled.claim_id)


def test_failed_marker_keeps_a_confirmed_stopped_cancel_open_until_publication_recovers(
    world: World, monkeypatch
) -> None:
    run_id, _, _ = _running(world)
    world.launcher.confirm_stopped(world.store.get_run(run_id))
    real_create = world.objects.create_only
    unavailable = True

    def publish(name: str, data: bytes) -> None:
        if name.endswith("/cancel.json") and unavailable:
            raise RuntimeError("object store unavailable")
        real_create(name, data)

    monkeypatch.setattr(world.objects, "create_only", publish)
    assert _cancel(world, run_id)["state"] == "cancel_requested"
    pending = world.store.get_run(run_id)
    assert pending.reserved_seconds > 0
    assert world.objects.get(_marker_name(world, run_id)) is None

    assert reconcile(world, run_id)["state"] == "cancel_requested"
    assert world.store.get_run(run_id) == pending
    actions = plan_sweep([pending], now=world.clock.now(), policy=make_policy())
    assert [action.run_id for action in actions] == [run_id]

    unavailable = False
    assert reconcile(world, run_id)["state"] == "cancelled"
    settled = world.store.get_run(run_id)
    assert settled.reserved_seconds == 0
    assert world.objects.get(_marker_name(world, run_id)) is not None
    assert reconcile(world, run_id)["state"] == "cancelled"
    assert world.store.get_run(run_id) == settled
