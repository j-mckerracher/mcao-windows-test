"""GET status reconciles the run's state, freshness, failure, effective pins, and result (DPES-815 AC2).

Status is read through the HTTP API after the worker side is replayed through runner plans, so the
answers come from the same records the lifecycle tests drive. Checkpoint availability is not
asserted here: its wire field is BLOCKING on DPES-869 and has no value to report yet.
"""

from fakes import IMAGE_DIGEST, OWNER_A, OWNER_B, make_policy, owner_headers
from flows import (
    World,
    admit_and_start,
    fail_launch_uncertain,
    make_world,
    reconcile,
    to_starting,
    write_status,
)
from mao_cloud.protocol import ArtifactRef, artifact_object_name, sha256_hex
from mao_cloud.runner import plan_result


def _status(world: World, run_id: str, owner: str = OWNER_A) -> dict[str, object]:
    response = world.client.get(f"/v1/runs/{run_id}/status", headers=owner_headers(owner))
    assert response.status_code == 200, response.text
    return response.json()


def test_status_reports_the_effective_model_resource_and_image_the_run_is_pinned_to(
    world: World,
) -> None:
    run_id, _, _ = admit_and_start(world)

    body = _status(world, run_id)

    assert body["effective"] == {
        "model": "gemini-3.8-flash",
        "resource_class": "standard",
        "image_digest": IMAGE_DIGEST,
    }


def test_result_is_unavailable_while_the_attempt_is_still_running(world: World) -> None:
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1, phase="running")
    assert reconcile(world, run_id)["state"] == "running"

    body = _status(world, run_id)

    assert body["run"]["state"] == "running"
    assert body["run"]["freshness"] == "fresh"
    assert body["result_available"] is False


def test_status_of_a_run_whose_heartbeat_stopped_reports_stale_freshness(world: World) -> None:
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1, phase="running")
    assert reconcile(world, run_id)["state"] == "running"

    world.clock.advance(seconds=121)

    assert _status(world, run_id)["run"]["freshness"] == "stale"


def test_an_accepted_worker_failure_is_reported_with_its_result_available(world: World) -> None:
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1, phase="running")
    assert reconcile(world, run_id)["state"] == "running"
    world.objects.put_planned(
        plan_result(bootstrap, outcome="failed", exit_code=2, commit_sha=None, artifacts=[])
    )
    assert reconcile(world, run_id)["state"] == "failed"

    body = _status(world, run_id)

    assert body["run"]["failure"]["code"] == "worker_failed"
    assert body["result_available"] is True


def test_a_succeeded_run_reports_its_result_as_available(world: World) -> None:
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1, phase="running")
    assert reconcile(world, run_id)["state"] == "running"
    report = b"all green\n"
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
    assert reconcile(world, run_id)["state"] == "succeeded"

    assert _status(world, run_id)["result_available"] is True


def test_a_missing_result_failure_reports_no_result_available(world: World) -> None:
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1, phase="running")
    assert reconcile(world, run_id)["state"] == "running"
    write_status(world, bootstrap, seq=2, phase="collecting")
    assert reconcile(world, run_id)["state"] == "collecting"
    world.launcher.confirm_stopped(world.store.get_run(run_id))
    assert reconcile(world, run_id)["state"] == "failed"

    body = _status(world, run_id)

    assert body["run"]["failure"]["code"] == "missing_result"
    assert body["result_available"] is False


def test_a_platform_failure_is_carried_in_status_with_its_typed_code(world: World) -> None:
    run_id, _ = fail_launch_uncertain(world)

    body = _status(world, run_id)

    assert body["run"]["state"] == "failed"
    assert body["run"]["failure"]["code"] == "launch_uncertain"
    assert body["result_available"] is False


def test_status_is_still_scoped_to_the_owner(world: World) -> None:
    run_id, _, _ = admit_and_start(world)

    response = world.client.get(f"/v1/runs/{run_id}/status", headers=owner_headers(OWNER_B))

    assert response.status_code == 404
    assert response.json()["error"]["code"] == "not_found"


def test_a_ninety_second_policy_marks_a_silent_run_stale_just_past_ninety_seconds() -> None:
    world = make_world(policy=make_policy(stale_after_seconds=90))
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1, phase="running")
    assert reconcile(world, run_id)["state"] == "running"

    world.clock.advance(seconds=90)
    assert _status(world, run_id)["run"]["freshness"] == "fresh"

    world.clock.advance(seconds=1)
    assert _status(world, run_id)["run"]["freshness"] == "stale"
