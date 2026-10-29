"""Log cursors over HTTP: trimmed history, the canonical v1 token, and an unsupported version.

Only cursor version 1 exists, so older-version identity is pinned with literal tokens written
by hand from the protocol's cursor fields, not produced by encode_cursor. The literal form is
the canonical bytes: sorted keys, no whitespace, base64url with no padding.
"""

import base64
from typing import Any

from fakes import owner_headers
from flows import (
    World,
    admit_and_start,
    reconcile,
    to_starting,
    view,
    write_status,
)
from mao_cloud.protocol import ObjectKind, object_name
from mao_cloud.runner import plan_log_entry


def _running_with_logs(world: World) -> str:
    """A running attempt with log entries 1 to 3 and a watermark of 3."""
    run_id, _, receipt = admit_and_start(world, budget={"max_runtime_seconds": 3600})
    bootstrap = to_starting(world, run_id, receipt)
    for seq in (1, 2, 3):
        world.objects.put_planned(plan_log_entry(bootstrap, seq=seq, lines=[f"line {seq}"]))
    write_status(world, bootstrap, seq=1, log_watermark=3)
    assert reconcile(world, run_id)["state"] == "running"
    return run_id


def _trim(world: World, run_id: str, *, retained_from: int, deleted: tuple[int, ...]) -> None:
    """Model retention: the record's retained floor moves up and the older objects are removed."""
    record = world.store.get_run(run_id)
    assert record is not None
    world.store.compare_and_set(
        record.model_copy(update={"log_retained_from": retained_from}),
        expected_version=record.version,
    )
    for seq in deleted:
        world.objects.delete(object_name(ObjectKind.LOG, run_id=run_id, attempt=1, seq=seq))


def _logs(world: World, run_id: str, **params: Any) -> Any:
    return world.client.get(f"/v1/runs/{run_id}/logs", params=params, headers=owner_headers())


def _token(payload: str) -> str:
    """A hand-written cursor token: base64url of the literal canonical JSON, unpadded."""
    return base64.urlsafe_b64encode(payload.encode("ascii")).rstrip(b"=").decode("ascii")


# -- trimmed history -----------------------------------------------------------


def test_trimmed_history_returns_retained_entries_with_the_gap_disclosed(world: World) -> None:
    run_id = _running_with_logs(world)
    _trim(world, run_id, retained_from=3, deleted=(1, 2))

    response = _logs(world, run_id)
    assert response.status_code == 200, response.text
    page = response.json()
    assert [entry["seq"] for entry in page["entries"]] == [3]
    assert page["trimmed"] is True
    assert page["gap"] == {"from_seq": 1, "to_seq": 2}
    assert page["retained_from"] == 3
    assert page["watermark"] == 3
    assert page["snapshot"]["state"] == "running"
    assert page["snapshot"]["attempt"] == 1


def test_continuation_after_a_trimmed_page_returns_an_empty_page_with_no_gap(world: World) -> None:
    run_id = _running_with_logs(world)
    _trim(world, run_id, retained_from=3, deleted=(1, 2))

    first = _logs(world, run_id).json()
    assert first["gap"] == {"from_seq": 1, "to_seq": 2}

    continued = _logs(world, run_id, cursor=first["next_cursor"])
    assert continued.status_code == 200, continued.text
    page = continued.json()
    assert page["entries"] == []
    assert page["trimmed"] is False
    assert page["gap"] is None
    assert page["next_cursor"] == first["next_cursor"]


def test_trimmed_page_snapshot_and_watermark_agree_with_the_status_view(world: World) -> None:
    run_id = _running_with_logs(world)
    _trim(world, run_id, retained_from=3, deleted=(1, 2))

    page = _logs(world, run_id).json()
    status = world.client.get(f"/v1/runs/{run_id}/status", headers=owner_headers()).json()

    assert page["trimmed"] is True
    assert page["snapshot"]["state"] == status["run"]["state"] == "running"
    assert page["snapshot"]["version"] == status["run"]["version"]
    assert page["snapshot"]["attempt"] == status["run"]["attempt"]
    assert page["watermark"] == status["log_watermark"] == 3
    assert page["retained_from"] == status["retained_from"] == 3


# -- the canonical v1 token ----------------------------------------------------


def test_hand_written_v1_cursor_resumes_at_its_position_with_canonical_next_cursor(
    world: World,
) -> None:
    run_id = _running_with_logs(world)
    token = _token('{"a":1,"q":2,"r":"%s","s":"logs","v":1}' % run_id)

    response = _logs(world, run_id, cursor=token)
    assert response.status_code == 200, response.text
    page = response.json()
    assert [entry["seq"] for entry in page["entries"]] == [2, 3]
    assert page["next_cursor"] == _token('{"a":1,"q":4,"r":"%s","s":"logs","v":1}' % run_id)


# -- an unsupported version ----------------------------------------------------


def test_v0_cursor_is_refused_with_its_version_and_the_run_stays_inspectable(
    world: World,
) -> None:
    run_id = _running_with_logs(world)
    token = _token('{"a":1,"q":1,"r":"%s","s":"logs","v":0}' % run_id)

    response = _logs(world, run_id, cursor=token)
    assert response.status_code == 400, response.text
    error = response.json()["error"]
    assert error["code"] == "cursor_version_unsupported"
    assert error["detail"]["version"] == 0
    assert view(world, run_id)["state"] == "running"
