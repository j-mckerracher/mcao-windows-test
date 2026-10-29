"""Happy-path run lifecycle over the HTTP API, with the worker side replayed through runner plans."""

import uuid

from fakes import (
    OWNER_A,
    OWNER_B,
    create_payload,
    owner_headers,
    start_payload,
)
from flows import (
    World,
    admit_and_start,
    reconcile,
    to_starting,
    write_status,
)
from mao_cloud.protocol import (
    AcceptedReceipt,
    ArtifactRef,
    ObjectKind,
    RunResultDocument,
    artifact_object_name,
    object_name,
    sha256_hex,
)
from mao_cloud.runner import plan_log_entry, plan_result


def test_lifecycle_runs_from_admission_to_succeeded(world: World) -> None:
    run_id, _, receipt = admit_and_start(world)
    assert receipt.state.value == "queued"

    bootstrap = to_starting(world, run_id, receipt)
    world.objects.put_planned(
        plan_log_entry(bootstrap, seq=1, lines=["install ok", "tests collected"])
    )
    write_status(world, bootstrap, seq=1, log_watermark=1)
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

    result = world.client.get(f"/v1/runs/{run_id}/result", headers=owner_headers())
    assert result.status_code == 200, result.text
    document = RunResultDocument.model_validate(result.json())
    assert document.outcome == "succeeded"
    assert document.artifacts == (artifact,)


def test_owner_cannot_see_another_owners_run(world: World) -> None:
    run_id, _, _ = admit_and_start(world, owner=OWNER_A)
    other = world.client.get(f"/v1/runs/{run_id}", headers=owner_headers(OWNER_B))
    assert other.status_code == 404
    assert other.json()["error"]["code"] == "not_found"


def test_missing_owner_identity_is_refused(world: World) -> None:
    response = world.client.post("/v1/runs", json=create_payload())
    assert response.status_code == 403
    assert response.json()["error"]["code"] == "not_allowed"


def test_internal_reconcile_requires_controller_identity(world: World) -> None:
    run_id, _, _ = admit_and_start(world)
    response = world.client.post(
        f"/internal/v1/runs/{run_id}:reconcile", headers=owner_headers()
    )
    assert response.status_code == 403
    assert response.json()["error"]["code"] == "not_allowed"
    assert world.launcher.launched == []


def test_result_before_terminal_state_is_not_ready(world: World) -> None:
    run_id, _, _ = admit_and_start(world)
    response = world.client.get(f"/v1/runs/{run_id}/result", headers=owner_headers())
    assert response.status_code == 409
    assert response.json()["error"]["code"] == "result_not_ready"


def test_logs_page_forward_with_opaque_cursor(world: World) -> None:
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    for seq in (1, 2, 3):
        world.objects.put_planned(plan_log_entry(bootstrap, seq=seq, lines=[f"line {seq}"]))
    write_status(world, bootstrap, seq=1, log_watermark=3)
    assert reconcile(world, run_id)["state"] == "running"

    first = world.client.get(f"/v1/runs/{run_id}/logs?limit=2", headers=owner_headers())
    assert first.status_code == 200, first.text
    page = first.json()
    assert [entry["seq"] for entry in page["entries"]] == [1, 2]
    assert page["watermark"] == 3
    assert page["trimmed"] is False
    assert page["gap"] is None

    second = world.client.get(
        f"/v1/runs/{run_id}/logs",
        params={"cursor": page["next_cursor"], "limit": 2},
        headers=owner_headers(),
    )
    assert second.status_code == 200, second.text
    assert [entry["seq"] for entry in second.json()["entries"]] == [3]


def test_start_replay_returns_the_same_receipt(world: World) -> None:
    run_id, create, receipt = admit_and_start(world)
    generation = world.objects.generation_of(object_name(ObjectKind.INPUT, run_id=run_id))
    again = world.client.post(
        f"/v1/runs/{run_id}:start",
        json=start_payload(create, generation=generation),
        headers=owner_headers(),
    )
    assert again.status_code == 200, again.text
    assert AcceptedReceipt.model_validate(again.json()) == receipt


def test_create_replay_returns_existing_run_with_its_upload(world: World) -> None:
    create = create_payload()
    first = world.client.post("/v1/runs", json=create, headers=owner_headers())
    second = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert first.status_code == 201, first.text
    assert second.status_code == 200, second.text
    run_id = first.json()["run"]["run_id"]
    assert second.json()["run"]["run_id"] == run_id
    upload = second.json()["upload"]
    assert upload is not None
    assert upload["object_name"] == object_name(ObjectKind.INPUT, run_id=run_id)


def test_create_with_changed_digest_conflicts(world: World) -> None:
    create = create_payload()
    assert world.client.post("/v1/runs", json=create, headers=owner_headers()).status_code == 201
    changed = create_payload(
        request_uuid=uuid.UUID(create["request_uuid"]),
        budget={"max_runtime_seconds": 900},
    )
    response = world.client.post("/v1/runs", json=changed, headers=owner_headers())
    assert response.status_code == 409
    assert response.json()["error"]["code"] == "request_digest_conflict"


def test_disallowed_target_is_refused_before_any_run_exists(world: World) -> None:
    create = create_payload(target={"id": "other-target"})
    response = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert response.status_code == 403
    assert response.json()["error"]["code"] == "not_allowed"
    assert world.store.get_idempotency(OWNER_A, uuid.UUID(create["request_uuid"])) is None


def test_result_write_carries_its_checksum_and_create_only_precondition(world: World) -> None:
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    write = plan_result(bootstrap, outcome="failed", exit_code=2, commit_sha=None, artifacts=[])
    assert write.name == object_name(ObjectKind.RESULT, run_id=run_id, attempt=1)
    assert write.headers["x-goog-if-generation-match"] == "0"
    assert write.headers["x-goog-meta-sha256"] == sha256_hex(write.body)
    assert write.headers["content-type"] == "application/json"
