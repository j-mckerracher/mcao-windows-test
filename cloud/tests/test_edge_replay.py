"""Replays over HTTP after the idempotency window, lost responses, partial cleanup, and restore.

A replayed request UUID must come back as the stored run, a typed refusal, or a conflict. It must
never open a second run or a second attempt. Each test replays a request the broker has already
answered, so the only thing that differs from the first call is the state around it.
"""

import uuid

from fakes import OWNER_A, create_payload, owner_headers, start_payload
from flows import (
    LONG_BUDGET,
    World,
    admit_and_start,
    fail_launch_uncertain,
    post_retry,
    reconcile,
    retry_body,
    to_starting,
    view,
    write_status,
)
from mao_cloud.protocol import AcceptedReceipt, ObjectKind, object_name
from mao_cloud.runner import plan_log_entry

# -- the seven-day window -----------------------------------------------------


def test_retry_replayed_after_seven_days_is_expired_and_opens_no_attempt(world: World) -> None:
    run_id, _ = fail_launch_uncertain(world)
    body = retry_body(world, expected_version=view(world, run_id)["version"])
    first = post_retry(world, run_id, body)
    assert first.status_code == 200, first.text
    applied = view(world, run_id)
    assert applied["attempt"] == 2

    world.clock.advance(days=7)
    replay = post_retry(world, run_id, body)
    assert replay.status_code == 410, replay.text
    assert replay.json()["error"]["code"] == "idempotency_expired"
    assert replay.json()["error"]["detail"]["reason"] == "ttl"

    after = view(world, run_id)
    assert after["attempt"] == 2
    assert after["version"] == applied["version"]
    assert len(world.launcher.launched) == 1
    assert world.store.run_count() == 1


def test_retry_with_a_changed_body_after_expiry_is_a_digest_conflict_not_an_expiry(
    world: World,
) -> None:
    run_id, _ = fail_launch_uncertain(world)
    body = retry_body(world, expected_version=view(world, run_id)["version"])
    assert post_retry(world, run_id, body).status_code == 200

    world.clock.advance(days=7)
    changed = {**body, "expected_version": body["expected_version"] + 5}
    response = post_retry(world, run_id, changed)
    assert response.status_code == 409, response.text
    assert response.json()["error"]["code"] == "request_digest_conflict"
    assert view(world, run_id)["attempt"] == 2


def test_create_replayed_with_a_changed_manifest_after_expiry_is_a_digest_conflict(
    world: World,
) -> None:
    create = create_payload()
    assert world.client.post("/v1/runs", json=create, headers=owner_headers()).status_code == 201

    world.clock.advance(days=7)
    changed = create_payload(
        request_uuid=uuid.UUID(create["request_uuid"]),
        budget={"max_runtime_seconds": 300},
    )
    response = world.client.post("/v1/runs", json=changed, headers=owner_headers())
    assert response.status_code == 409, response.text
    assert response.json()["error"]["code"] == "request_digest_conflict"
    assert world.store.run_count() == 1


# -- a dedup row lost while its run survives ----------------------------------


def test_retry_replay_whose_dedup_row_is_lost_is_refused_and_not_applied_again(
    world: World,
) -> None:
    run_id, _ = fail_launch_uncertain(world)
    body = retry_body(world, expected_version=view(world, run_id)["version"])
    assert post_retry(world, run_id, body).status_code == 200
    world.store.drop_idempotency(OWNER_A, uuid.UUID(body["request_uuid"]))

    replay = post_retry(world, run_id, body)
    assert replay.status_code == 410, replay.text
    error = replay.json()["error"]
    assert error["code"] == "idempotency_expired"
    assert error["detail"]["reason"] == "record_missing_run_present"
    assert error["detail"]["run_id"] == run_id
    assert view(world, run_id)["attempt"] == 2
    assert len(world.launcher.launched) == 1


# -- lost responses and partial cleanup ---------------------------------------


def test_lost_create_and_start_responses_replay_the_same_run_after_progress(world: World) -> None:
    run_id, create, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1)
    assert reconcile(world, run_id)["state"] == "running"

    replayed_create = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert replayed_create.status_code == 200, replayed_create.text
    assert replayed_create.json()["run"]["run_id"] == run_id

    generation = world.objects.generation_of(object_name(ObjectKind.INPUT, run_id=run_id))
    replayed_start = world.client.post(
        f"/v1/runs/{run_id}:start",
        json=start_payload(create, generation=generation),
        headers=owner_headers(),
    )
    assert replayed_start.status_code == 200, replayed_start.text
    assert AcceptedReceipt.model_validate(replayed_start.json()) == receipt
    assert world.store.run_count() == 1
    assert len(world.launcher.launched) == 1


def test_replay_after_partial_cleanup_returns_the_same_run_and_receipt(world: World) -> None:
    run_id, create, receipt = admit_and_start(world, budget=LONG_BUDGET)
    bootstrap = to_starting(world, run_id, receipt)
    for seq in (1, 2, 3):
        world.objects.put_planned(plan_log_entry(bootstrap, seq=seq, lines=[f"line {seq}"]))
    write_status(world, bootstrap, seq=1, log_watermark=3)
    assert reconcile(world, run_id)["state"] == "running"

    input_name = object_name(ObjectKind.INPUT, run_id=run_id)
    generation = world.objects.generation_of(input_name)
    world.objects.delete(input_name)
    for seq in (1, 2, 3):
        world.objects.delete(object_name(ObjectKind.LOG, run_id=run_id, attempt=1, seq=seq))

    replayed_create = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert replayed_create.status_code == 200, replayed_create.text
    assert replayed_create.json()["run"]["run_id"] == run_id

    replayed_start = world.client.post(
        f"/v1/runs/{run_id}:start",
        json=start_payload(create, generation=generation),
        headers=owner_headers(),
    )
    assert replayed_start.status_code == 200, replayed_start.text
    assert AcceptedReceipt.model_validate(replayed_start.json()) == receipt

    # The missing objects surface as a typed refusal on read, not a silently short page.
    logs = world.client.get(f"/v1/runs/{run_id}/logs", headers=owner_headers())
    assert logs.status_code == 404
    assert logs.json()["error"]["code"] == "not_found"
    assert view(world, run_id)["state"] == "running"
    assert world.store.run_count() == 1


# -- backup restore beyond the admission floor --------------------------------
# StoreBackup is a point-in-time copy of the in-memory run and dedup tables. It is fake-store
# evidence only; it is not a Firestore backup or restore drill.


def test_restored_dedup_row_still_replays_the_stored_run_after_restore_completes(
    world: World,
) -> None:
    run_id, create, _ = admit_and_start(world, budget=LONG_BUDGET)
    backup = world.store.backup()

    world.clock.advance(minutes=30)
    world.store.restore(backup, floor=world.clock.now())

    replay = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert replay.status_code == 200, replay.text
    assert replay.json()["run"]["run_id"] == run_id
    assert world.store.run_count() == 1


def test_retry_applied_before_backup_replays_the_same_attempt_after_restore(
    world: World,
) -> None:
    run_id, _ = fail_launch_uncertain(world)
    body = retry_body(world, expected_version=view(world, run_id)["version"])
    applied = post_retry(world, run_id, body)
    assert applied.status_code == 200, applied.text
    backup = world.store.backup()

    world.clock.advance(minutes=10)
    world.store.restore(backup, floor=world.clock.now())
    assert view(world, run_id)["attempt"] == 2

    replay = post_retry(world, run_id, body)
    assert replay.status_code == 200, replay.text
    assert replay.json()["attempt"] == 2
    assert view(world, run_id)["attempt"] == 2
    assert world.store.run_count() == 1

    assert reconcile(world, run_id)["state"] == "dispatching"
    assert [bootstrap.attempt for bootstrap in world.launcher.launched] == [1, 2]


def test_retry_applied_after_backup_and_lost_is_refused_past_the_restore_floor(
    world: World,
) -> None:
    run_id, _ = fail_launch_uncertain(world)
    backup = world.store.backup()
    body = retry_body(world, expected_version=view(world, run_id)["version"])
    assert post_retry(world, run_id, body).status_code == 200
    assert view(world, run_id)["attempt"] == 2

    world.clock.advance(hours=1)
    world.store.restore(backup, floor=world.clock.now())
    assert view(world, run_id)["attempt"] == 1

    replay = post_retry(world, run_id, body)
    assert replay.status_code == 410, replay.text
    assert replay.json()["error"]["code"] == "idempotency_expired"
    assert replay.json()["error"]["detail"]["reason"] == "before_admission_floor"
    assert view(world, run_id)["attempt"] == 1
    assert len(world.launcher.launched) == 1
