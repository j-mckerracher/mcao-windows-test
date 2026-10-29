"""HTTP contract tests: cancel and retry, idempotency lifetime, document fencing, and cursors.

Worker documents are written through runner plans, the same bytes a real runner would send.
Liveness windows come from the test policy: stale after 120 seconds, startup timeout 600.
"""

import uuid
from datetime import timedelta
from typing import Any

import pytest

from fakes import (
    CONTROLLER_HEADERS,
    EPOCH,
    OWNER_A,
    OWNER_B,
    create_payload,
    owner_headers,
    uuid7,
)
from flows import (
    World,
    admit_and_start,
    claim,
    dispatch,
    reconcile,
    to_starting,
    write_status,
)
from mao_cloud.protocol import (
    AcceptedReceipt,
    ArtifactRef,
    Cursor,
    CursorStream,
    ObjectKind,
    artifact_object_name,
    encode_cursor,
    object_name,
    sha256_hex,
)
from mao_cloud.runner import plan_log_entry, plan_result

LONG_BUDGET = {"max_runtime_seconds": 3600}
CANCEL_BODY = {"protocol_version": "v1"}
UNKNOWN_RUN_ID = "run_" + "f" * 32


def _view(world: World, run_id: str, owner: str = OWNER_A) -> dict[str, Any]:
    response = world.client.get(f"/v1/runs/{run_id}", headers=owner_headers(owner))
    assert response.status_code == 200, response.text
    return response.json()


def _retry(world: World, run_id: str, *, expected_version: int, expected_attempt: int = 1) -> Any:
    body = {
        "protocol_version": "v1",
        "request_uuid": str(uuid7(world.clock.now())),
        "expected_attempt": expected_attempt,
        "expected_version": expected_version,
    }
    return world.client.post(f"/v1/runs/{run_id}:retry", json=body, headers=owner_headers())


def _fail_launch_uncertain(world: World) -> tuple[str, AcceptedReceipt]:
    """Dispatch, let the launch go silent past startup and the stale window, then fail it."""
    run_id, _, receipt = admit_and_start(world, budget=LONG_BUDGET)
    _drive_to_launch_uncertain(world, run_id)
    return run_id, receipt


def _drive_to_launch_uncertain(world: World, run_id: str) -> None:
    dispatch(world, run_id)
    world.clock.advance(seconds=601)
    assert reconcile(world, run_id)["state"] == "reconciling"
    world.clock.advance(seconds=121)
    view = reconcile(world, run_id)
    assert view["state"] == "failed"
    assert view["failure"]["code"] == "launch_uncertain"
    world.launcher.confirm_stopped(world.store.get_run(run_id))
    reconcile(world, run_id)


def _fail_heartbeat_lost(world: World) -> str:
    """A running attempt writes logs and one status, then stops reporting."""
    run_id, _, receipt = admit_and_start(world, budget=LONG_BUDGET)
    bootstrap = to_starting(world, run_id, receipt)
    for seq in (1, 2, 3):
        world.objects.put_planned(plan_log_entry(bootstrap, seq=seq, lines=[f"line {seq}"]))
    write_status(world, bootstrap, seq=1, log_watermark=3)
    assert reconcile(world, run_id)["state"] == "running"
    world.clock.advance(seconds=121)
    assert reconcile(world, run_id)["state"] == "reconciling"
    world.clock.advance(seconds=121)
    view = reconcile(world, run_id)
    assert view["state"] == "failed"
    assert view["failure"]["code"] == "heartbeat_lost"
    world.launcher.confirm_stopped(world.store.get_run(run_id))
    reconcile(world, run_id)
    return run_id


# -- cancel -------------------------------------------------------------------


def test_cancel_before_upload_is_cancelled_at_once(world: World) -> None:
    created = world.client.post("/v1/runs", json=create_payload(), headers=owner_headers())
    run_id = created.json()["run"]["run_id"]
    response = world.client.post(
        f"/v1/runs/{run_id}:cancel", json=CANCEL_BODY, headers=owner_headers()
    )
    assert response.status_code == 200, response.text
    assert response.json()["state"] == "cancelled"
    assert response.json()["failure"]["code"] == "owner_cancelled"


def test_cancel_of_running_run_waits_until_the_worker_goes_quiet(world: World) -> None:
    run_id, _, receipt = admit_and_start(world, budget=LONG_BUDGET)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1)
    assert reconcile(world, run_id)["state"] == "running"

    response = world.client.post(
        f"/v1/runs/{run_id}:cancel", json=CANCEL_BODY, headers=owner_headers()
    )
    assert response.json()["state"] == "cancel_requested"
    assert reconcile(world, run_id)["state"] == "cancel_requested"

    # A silent worker does not complete the cancel: only confirmed termination does.
    world.clock.advance(seconds=121)
    assert reconcile(world, run_id)["state"] == "cancel_requested"

    world.launcher.confirm_stopped(world.store.get_run(run_id))
    final = reconcile(world, run_id)
    assert final["state"] == "cancelled"
    assert final["failure"]["code"] == "owner_cancelled"


def test_cancel_while_dispatching_waits_for_confirmed_stop_before_cancelling(world: World) -> None:
    run_id, _, _ = admit_and_start(world)
    dispatch(world, run_id)
    response = world.client.post(
        f"/v1/runs/{run_id}:cancel", json=CANCEL_BODY, headers=owner_headers()
    )
    assert response.json()["state"] == "cancel_requested"
    # No claim has arrived, so last_seen is None and freshness is stale. The launch may still
    # exist, so the cancel stays open until the launcher confirms nothing can run.
    assert reconcile(world, run_id)["state"] == "cancel_requested"

    world.launcher.confirm_stopped(world.store.get_run(run_id))
    assert reconcile(world, run_id)["state"] == "cancelled"


def test_other_owner_cannot_cancel_a_run(world: World) -> None:
    run_id, _, _ = admit_and_start(world, owner=OWNER_A)
    response = world.client.post(
        f"/v1/runs/{run_id}:cancel", json=CANCEL_BODY, headers=owner_headers(OWNER_B)
    )
    assert response.status_code == 404
    assert _view(world, run_id)["state"] == "queued"


def test_launcher_failure_leaves_run_dispatching_and_is_not_relaunched(world: World) -> None:
    run_id, _, _ = admit_and_start(world)
    world.launcher.fail_next = True
    with pytest.raises(RuntimeError, match="launcher unavailable"):
        world.client.post(f"/internal/v1/runs/{run_id}:reconcile", headers=CONTROLLER_HEADERS)
    assert world.store.get_run(run_id).state.value == "dispatching"
    assert world.launcher.launched == []

    assert reconcile(world, run_id)["state"] == "dispatching"
    assert world.launcher.launched == []


# -- retry --------------------------------------------------------------------


def test_retry_after_launch_uncertain_reopens_attempt_under_the_original_deadline(
    world: World,
) -> None:
    run_id, _ = _fail_launch_uncertain(world)
    before = _view(world, run_id)

    response = _retry(world, run_id, expected_version=before["version"])
    assert response.status_code == 200, response.text
    after = _view(world, run_id)
    assert after["state"] == "queued"
    assert after["attempt"] == 2
    assert after["deadline_at"] == before["deadline_at"]

    assert reconcile(world, run_id)["state"] == "dispatching"
    relaunched = world.launcher.launched[-1]
    assert relaunched.attempt == 2
    assert relaunched.fence == 2


def test_retry_after_heartbeat_lost_reopens_the_attempt(world: World) -> None:
    run_id = _fail_heartbeat_lost(world)
    version = _view(world, run_id)["version"]
    response = _retry(world, run_id, expected_version=version)
    assert response.status_code == 200, response.text
    assert _view(world, run_id)["attempt"] == 2


def test_retry_replay_with_the_same_request_uuid_does_not_open_another_attempt(
    world: World,
) -> None:
    run_id, _ = _fail_launch_uncertain(world)
    before = _view(world, run_id)
    body = {
        "protocol_version": "v1",
        "request_uuid": str(uuid7(world.clock.now())),
        "expected_attempt": 1,
        "expected_version": before["version"],
    }
    first = world.client.post(f"/v1/runs/{run_id}:retry", json=body, headers=owner_headers())
    assert first.status_code == 200, first.text

    replay = world.client.post(f"/v1/runs/{run_id}:retry", json=body, headers=owner_headers())
    assert replay.status_code == 200, replay.text
    assert replay.json()["attempt"] == 2
    assert _view(world, run_id)["attempt"] == 2


def test_create_replay_after_a_retry_still_returns_the_same_run(world: World) -> None:
    run_id, create, _ = admit_and_start(world, budget=LONG_BUDGET)
    _drive_to_launch_uncertain(world, run_id)
    version = _view(world, run_id)["version"]
    assert _retry(world, run_id, expected_version=version).status_code == 200

    replay = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert replay.status_code < 300, replay.text
    assert replay.json()["run"]["run_id"] == run_id
    assert _view(world, run_id)["attempt"] == 2


def test_retry_reusing_a_request_uuid_with_a_different_body_is_a_digest_conflict(
    world: World,
) -> None:
    run_id, _ = _fail_launch_uncertain(world)
    before = _view(world, run_id)
    request_uuid = str(uuid7(world.clock.now()))
    body = {
        "protocol_version": "v1",
        "request_uuid": request_uuid,
        "expected_attempt": 1,
        "expected_version": before["version"],
    }
    first = world.client.post(f"/v1/runs/{run_id}:retry", json=body, headers=owner_headers())
    assert first.status_code == 200, first.text

    changed = {**body, "expected_version": before["version"] + 5}
    conflict = world.client.post(f"/v1/runs/{run_id}:retry", json=changed, headers=owner_headers())
    assert conflict.status_code == 409
    assert conflict.json()["error"]["code"] == "request_digest_conflict"
    assert _view(world, run_id)["attempt"] == 2


def test_retry_with_a_stale_version_conflicts(world: World) -> None:
    run_id, _ = _fail_launch_uncertain(world)
    stale = _view(world, run_id)["version"] - 1
    response = _retry(world, run_id, expected_version=stale)
    assert response.status_code == 409
    assert response.json()["error"]["code"] == "version_conflict"
    assert _view(world, run_id)["attempt"] == 1


def test_retry_of_a_checksum_mismatch_is_refused(world: World) -> None:
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    report = b"all green\n"
    world.objects.write(artifact_object_name(run_id, 1, "report.txt"), b"all gr3en\n")
    artifact = ArtifactRef(name="report.txt", sha256=sha256_hex(report), size_bytes=len(report))
    world.objects.put_planned(
        plan_result(bootstrap, outcome="succeeded", exit_code=0, commit_sha="b" * 40, artifacts=[artifact])
    )
    view = reconcile(world, run_id)
    assert view["state"] == "failed"
    assert view["failure"]["code"] == "checksum_mismatch"

    response = _retry(world, run_id, expected_version=view["version"])
    assert response.status_code == 409
    assert response.json()["error"]["code"] == "retry_not_allowed"


# -- idempotency lifetime -----------------------------------------------------


def test_create_replay_after_seven_days_is_expired(world: World) -> None:
    create = create_payload()
    assert world.client.post("/v1/runs", json=create, headers=owner_headers()).status_code == 201
    world.clock.advance(days=7)
    response = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert response.status_code == 410
    assert response.json()["error"]["code"] == "idempotency_expired"
    assert response.json()["error"]["detail"]["reason"] == "ttl"


def test_create_replay_with_lost_dedup_record_is_refused_not_recreated(world: World) -> None:
    create = create_payload()
    first = world.client.post("/v1/runs", json=create, headers=owner_headers())
    run_id = first.json()["run"]["run_id"]
    world.store.drop_idempotency(OWNER_A, uuid.UUID(create["request_uuid"]))

    response = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert response.status_code == 410
    detail = response.json()["error"]["detail"]
    assert detail["reason"] == "record_missing_run_present"
    assert detail["run_id"] == run_id


def test_request_uuid_stamped_an_hour_ahead_is_invalid(world: World) -> None:
    create = create_payload(at=EPOCH + timedelta(hours=1))
    response = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert response.status_code == 400
    assert response.json()["error"]["code"] == "invalid_request"


def test_request_uuid_older_than_the_window_is_expired(world: World) -> None:
    create = create_payload(at=EPOCH - timedelta(days=8))
    response = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert response.status_code == 410
    assert response.json()["error"]["detail"]["reason"] == "before_admission_floor"


def test_restore_floor_above_the_uuid_stamp_refuses_the_request(world: World) -> None:
    create = create_payload()
    world.store.set_restore_floor(EPOCH + timedelta(minutes=1))
    response = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert response.status_code == 410
    assert response.json()["error"]["detail"]["reason"] == "before_admission_floor"
    assert world.store.get_idempotency(OWNER_A, uuid.UUID(create["request_uuid"])) is None


# -- document fencing and liveness --------------------------------------------


def test_superseded_attempt_result_at_the_new_path_is_ignored(world: World) -> None:
    run_id, receipt = _fail_launch_uncertain(world)
    first = world.launcher.launched[0]
    version = _view(world, run_id)["version"]
    assert _retry(world, run_id, expected_version=version).status_code == 200

    second = dispatch(world, run_id)
    assert second.attempt == 2
    claim(world, second, receipt)

    # Attempt 1's result bytes, placed at attempt 2's result name. Its attempt and fence are
    # stale, so ingest ignores it and the run cannot succeed on it.
    forged = plan_result(first, outcome="succeeded", exit_code=0, commit_sha="b" * 40, artifacts=[])
    world.objects.write(object_name(ObjectKind.RESULT, run_id=run_id, attempt=2), forged.body)

    assert reconcile(world, run_id)["state"] == "starting"
    result = world.client.get(f"/v1/runs/{run_id}/result", headers=owner_headers())
    assert result.status_code == 409
    assert result.json()["error"]["code"] == "result_not_ready"


def test_unreadable_status_document_does_not_block_liveness(world: World) -> None:
    run_id, _, receipt = admit_and_start(world, budget=LONG_BUDGET)
    to_starting(world, run_id, receipt)
    world.objects.write(object_name(ObjectKind.STATUS, run_id=run_id, attempt=1), b"{not json")
    world.clock.advance(seconds=121)
    assert reconcile(world, run_id)["state"] == "reconciling"


# -- cursors ------------------------------------------------------------------


def test_cursor_from_a_previous_attempt_is_refused_after_retry(world: World) -> None:
    run_id = _fail_heartbeat_lost(world)
    page = world.client.get(f"/v1/runs/{run_id}/logs?limit=2", headers=owner_headers()).json()
    assert [entry["seq"] for entry in page["entries"]] == [1, 2]

    version = _view(world, run_id)["version"]
    assert _retry(world, run_id, expected_version=version).status_code == 200

    response = world.client.get(
        f"/v1/runs/{run_id}/logs",
        params={"cursor": page["next_cursor"]},
        headers=owner_headers(),
    )
    assert response.status_code == 400
    assert response.json()["error"]["code"] == "cursor_invalid"


def test_unsupported_cursor_version_is_refused_and_run_stays_readable(world: World) -> None:
    run_id, _, _ = admit_and_start(world)
    token = encode_cursor(
        Cursor(run_id=run_id, attempt=1, stream=CursorStream.LOGS, seq=1),
        version=2,
    )
    response = world.client.get(
        f"/v1/runs/{run_id}/logs", params={"cursor": token}, headers=owner_headers()
    )
    assert response.status_code == 400
    assert response.json()["error"]["code"] == "cursor_version_unsupported"
    assert _view(world, run_id)["state"] == "queued"


def test_reconcile_of_unknown_run_is_not_found(world: World) -> None:
    response = world.client.post(
        f"/internal/v1/runs/{UNKNOWN_RUN_ID}:reconcile", headers=CONTROLLER_HEADERS
    )
    assert response.status_code == 404
    assert response.json()["error"]["code"] == "not_found"
