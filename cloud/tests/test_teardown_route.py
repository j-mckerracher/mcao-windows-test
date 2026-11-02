"""Teardown over HTTP: GET /v1/runs/{run_id}/teardown reports cleanup without deleting anything.

The route reports only the outcomes its facts can prove: running, fenced (capability expiry not yet
passed), and eligible (data present, deletion not started). complete, expired, incomplete, and
retained-exception are not reachable from the live route yet. Presence cannot be proven absent
without an object listing or a durable checkpoint-id index, and no retention exception or deletion
source exists. Those outcomes are pinned by the classifier tests in test_teardown.py.
"""

from typing import Any

from fakes import OWNER_A, OWNER_B, create_payload, owner_headers
from flows import World, admit_and_start, reconcile, to_starting, view, write_status
from mao_cloud.protocol import ArtifactRef, artifact_object_name, sha256_hex
from mao_cloud.runner import plan_result

CANCEL_BODY = {"protocol_version": "v1"}
UNKNOWN_RUN_ID = "run_" + "f" * 32
ONE_YEAR_SECONDS = 365 * 24 * 3600


def _cancelled_run(world: World) -> str:
    created = world.client.post(
        "/v1/runs", json=create_payload(), headers=owner_headers()
    )
    run_id = created.json()["run"]["run_id"]
    cancelled = world.client.post(
        f"/v1/runs/{run_id}:cancel", json=CANCEL_BODY, headers=owner_headers()
    )
    assert cancelled.json()["state"] == "cancelled", cancelled.text
    return run_id


def _succeeded_attempt(world: World) -> tuple[str, dict[str, Any]]:
    """A succeeded run whose attempt the launcher has not yet confirmed stopped, so it still holds capacity."""
    run_id, create, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1)
    assert reconcile(world, run_id)["state"] == "running"

    report = b"all green\n"
    world.objects.write(artifact_object_name(run_id, 1, "report.txt"), report)
    artifact = ArtifactRef(
        name="report.txt", sha256=sha256_hex(report), size_bytes=len(report)
    )
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
    return run_id, create


def _succeeded_run(world: World) -> tuple[str, dict[str, Any]]:
    """A succeeded run after confirmed stop, so its capacity is settled and cleanup can be fenced."""
    run_id, create = _succeeded_attempt(world)
    world.launcher.confirm_stopped(world.store.get_run(run_id))
    assert reconcile(world, run_id)["state"] == "succeeded"
    return run_id, create


def _teardown(world: World, run_id: str, owner: str = OWNER_A) -> dict[str, object]:
    response = world.client.get(
        f"/v1/runs/{run_id}/teardown", headers=owner_headers(owner)
    )
    assert response.status_code == 200, response.text
    body: dict[str, object] = response.json()
    return body


def test_terminal_run_is_fenced_until_its_deadline_then_eligible_with_data_pending(
    world: World,
) -> None:
    run_id = _cancelled_run(world)

    before = _teardown(world, run_id)
    assert before["execution_state"] == "cancelled"
    assert before["data_cleanup"] == "fenced"
    assert before["pending_deletion"] is True

    world.clock.advance(seconds=ONE_YEAR_SECONDS)

    after = _teardown(world, run_id)
    assert after["execution_state"] == "cancelled"
    assert after["data_cleanup"] == "eligible"
    assert after["pending_deletion"] is True


def test_teardown_keeps_execution_state_separate_and_never_implies_a_fresh_run(
    world: World,
) -> None:
    run_id = _cancelled_run(world)

    body = _teardown(world, run_id)
    assert body["run_id"] == run_id
    assert body["preserved_records"] == ["idempotency_record", "audit_record"]
    assert body["fresh_run_implied"] is False


def test_running_run_reports_running_and_is_not_pending_deletion(world: World) -> None:
    run_id, _, receipt = admit_and_start(world, budget={"max_runtime_seconds": 3600})
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1)
    assert reconcile(world, run_id)["state"] == "running"

    body = _teardown(world, run_id)
    assert body["execution_state"] == "running"
    assert body["data_cleanup"] == "running"
    assert body["pending_deletion"] is False


def test_teardown_read_is_repeatable_and_leaves_the_run_unchanged(world: World) -> None:
    run_id = _cancelled_run(world)
    version_before = view(world, run_id)["version"]

    first = _teardown(world, run_id)
    second = _teardown(world, run_id)

    assert first == second
    assert view(world, run_id)["version"] == version_before


def test_non_owner_gets_the_same_refusal_as_an_unknown_run(world: World) -> None:
    run_id = _cancelled_run(world)

    foreign = world.client.get(
        f"/v1/runs/{run_id}/teardown", headers=owner_headers(OWNER_B)
    )
    unknown = world.client.get(
        f"/v1/runs/{UNKNOWN_RUN_ID}/teardown", headers=owner_headers(OWNER_B)
    )

    assert foreign.status_code == unknown.status_code == 404
    assert (
        foreign.json()["error"]["code"]
        == unknown.json()["error"]["code"]
        == "not_found"
    )


def test_succeeded_run_is_running_until_its_stop_is_confirmed(world: World) -> None:
    run_id, _ = _succeeded_attempt(world)

    unsettled = _teardown(world, run_id)
    assert unsettled["execution_state"] == "succeeded"
    assert unsettled["data_cleanup"] == "running"
    assert unsettled["pending_deletion"] is False

    world.clock.advance(seconds=ONE_YEAR_SECONDS)
    assert _teardown(world, run_id)["data_cleanup"] == "running"

    world.launcher.confirm_stopped(world.store.get_run(run_id))
    assert reconcile(world, run_id)["state"] == "succeeded"
    settled = _teardown(world, run_id)
    assert settled["data_cleanup"] == "eligible"
    assert settled["pending_deletion"] is True


def test_succeeded_run_is_fenced_until_its_deadline_then_eligible(world: World) -> None:
    run_id, _ = _succeeded_run(world)

    before = _teardown(world, run_id)
    assert before["execution_state"] == "succeeded"
    assert before["data_cleanup"] == "fenced"
    assert before["pending_deletion"] is True
    assert before["fresh_run_implied"] is False

    world.clock.advance(seconds=ONE_YEAR_SECONDS)

    after = _teardown(world, run_id)
    assert after["execution_state"] == "succeeded"
    assert after["data_cleanup"] == "eligible"
    assert after["pending_deletion"] is True


def test_create_replay_after_teardown_still_returns_the_same_run(world: World) -> None:
    run_id, create = _succeeded_run(world)
    assert _teardown(world, run_id)["data_cleanup"] == "fenced"

    replay = world.client.post("/v1/runs", json=create, headers=owner_headers())

    assert replay.status_code == 200, replay.text
    assert replay.json()["run"]["run_id"] == run_id
    assert world.store.run_count() == 1
    assert _teardown(world, run_id)["execution_state"] == "succeeded"


def test_terminal_run_waits_for_confirmed_stop_before_cleanup_is_eligible(
    world: World,
) -> None:
    run_id, _, _ = admit_and_start(world)
    world.launcher.ambiguous_next = True
    reconcile(world, run_id)
    world.clock.advance(seconds=121)
    assert reconcile(world, run_id)["state"] == "failed"
    world.clock.advance(seconds=ONE_YEAR_SECONDS)
    held = world.store.get_run(run_id)
    assert held is not None and held.reserved_seconds > 0

    before = _teardown(world, run_id)
    assert before["execution_state"] == "failed"
    assert before["data_cleanup"] == "running"
    assert before["pending_deletion"] is False

    world.launcher.confirm_stopped(held)
    assert reconcile(world, run_id)["state"] == "failed"
    settled = world.store.get_run(run_id)
    assert settled is not None and settled.reserved_seconds == 0

    after = _teardown(world, run_id)
    assert after["execution_state"] == "failed"
    assert after["data_cleanup"] == "eligible"
    assert after["pending_deletion"] is True
