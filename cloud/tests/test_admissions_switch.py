"""The admissions switch stops new entry to the queue and cancels nothing.

Closing it refuses create, start (queue entry) and retry before any write. It never gates cancel,
never cancels admitted work, and never changes what an existing create replay answers. The switch
is read on every admitting request, so reopening it takes effect on the next request.
"""

from __future__ import annotations

from fakes import (
    EPOCH,
    OWNER_A,
    SOURCE_TAR,
    create_payload,
    owner_headers,
    start_payload,
    uuid7,
)
from flows import admit_and_start, make_world, view


def _refused(response, *, code: str = "admissions_paused") -> None:
    assert response.status_code == 503, response.text
    assert response.json()["error"]["code"] == code


def test_closed_switch_refuses_a_new_run_before_anything_is_written() -> None:
    world = make_world()
    world.admissions.set_open(False)

    response = world.client.post("/v1/runs", json=create_payload(), headers=owner_headers())

    _refused(response)
    assert not world.store.admission_snapshot().records


def test_closed_switch_refuses_starting_an_admitted_run_and_leaves_it_unchanged() -> None:
    world = make_world()
    create = create_payload(source=SOURCE_TAR)
    created = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert created.status_code == 201, created.text
    run_id = created.json()["run"]["run_id"]
    name = created.json()["upload"]["object_name"]
    world.objects.write(name, SOURCE_TAR)
    before = view(world, run_id)

    world.admissions.set_open(False)
    refused = world.client.post(
        f"/v1/runs/{run_id}:start",
        json=start_payload(create, generation=world.objects.generation_of(name)),
        headers=owner_headers(OWNER_A),
    )

    _refused(refused)
    assert view(world, run_id) == before
    assert before["state"] == "awaiting_upload"


def test_closed_switch_refuses_retry_before_the_retry_is_applied() -> None:
    world = make_world()
    run_id, _, _ = admit_and_start(world)
    before = view(world, run_id)

    world.admissions.set_open(False)
    refused = world.client.post(
        f"/v1/runs/{run_id}:retry",
        json={
            "protocol_version": "v1",
            "request_uuid": str(uuid7(EPOCH)),
            "expected_attempt": 1,
            "expected_version": before["version"],
        },
        headers=owner_headers(OWNER_A),
    )

    _refused(refused)
    assert view(world, run_id) == before


def test_closed_switch_never_cancels_admitted_work() -> None:
    world = make_world()
    run_id, _, _ = admit_and_start(world)
    before = view(world, run_id)
    assert before["state"] == "queued"

    world.admissions.set_open(False)

    assert view(world, run_id) == before
    assert world.launcher.launched == []
    cancelled = world.client.post(
        f"/v1/runs/{run_id}:cancel",
        json={"protocol_version": "v1"},
        headers=owner_headers(OWNER_A),
    )
    assert cancelled.status_code == 200, cancelled.text


def test_closed_switch_still_replays_an_admitted_create() -> None:
    world = make_world()
    create = create_payload()
    first = world.client.post("/v1/runs", json=create, headers=owner_headers())
    assert first.status_code == 201, first.text

    world.admissions.set_open(False)
    replay = world.client.post("/v1/runs", json=create, headers=owner_headers())

    assert replay.status_code == 200, replay.text
    assert replay.json()["run"]["run_id"] == first.json()["run"]["run_id"]


def test_reopening_the_switch_admits_new_runs_on_the_next_request() -> None:
    world = make_world()
    world.admissions.set_open(False)
    _refused(world.client.post("/v1/runs", json=create_payload(), headers=owner_headers()))

    world.admissions.set_open(True)
    reopened = world.client.post("/v1/runs", json=create_payload(), headers=owner_headers())

    assert reopened.status_code == 201, reopened.text
