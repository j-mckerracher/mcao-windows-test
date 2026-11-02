"""The controller's batch reconcile route, the Scheduler tick (DPES-809 AC1).

POST /internal/v1/runs:reconcile plans the due runs with the sweeper, then reconciles each one through
the same per-run reconcile the single-run route uses. Every transition is a compare-and-set against the
record that run was read from, a conflict is reported for that run alone, and the response is sent only
after all of the batch's work has finished.
"""

from datetime import timedelta
from typing import Any

import pytest
from fastapi.testclient import TestClient
from fakes import (
    CONTROLLER_HEADERS,
    OWNER_A,
    InMemoryAdmissionSwitch,
    InMemoryObjectStore,
    InMemoryRunStore,
    ManualClock,
    RecordingLauncher,
    make_policy,
    make_ports,
    owner_headers,
)
from flows import LONG_BUDGET, World, admit_and_start, dispatch, make_world
from mao_cloud.broker import create_app
from mao_cloud.protocol import ErrorCode, ProtocolError, RunState
from mao_cloud.sweeper import reconcile_backoff_seconds

BATCH = "/internal/v1/runs:reconcile"


def _batch(
    world: World,
    *,
    params: dict[str, Any] | None = None,
    headers: dict[str, str] = CONTROLLER_HEADERS,
) -> Any:
    return world.client.post(BATCH, params=params, headers=headers)


def _body(response: Any) -> dict[str, Any]:
    assert response.status_code == 200, response.text
    return response.json()


def _outcomes(response: Any) -> dict[str, dict[str, Any]]:
    return {entry["run_id"]: entry for entry in _body(response)["reconciled"]}


def _past_deadline(world: World) -> str:
    """A dispatched run whose runtime budget has lapsed, so its next reconcile must stop it."""
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    dispatch(world, run_id)
    world.clock.advance(seconds=3700)
    return run_id


def test_a_caller_without_the_controller_identity_is_refused_and_no_run_moves(
    world: World,
) -> None:
    run_id = _past_deadline(world)
    before = world.store.get_run(run_id)

    response = _batch(world, headers=owner_headers(OWNER_A))

    assert response.status_code == 403
    assert response.json()["error"]["code"] == ErrorCode.NOT_ALLOWED.value
    assert world.store.get_run(run_id) == before
    assert world.launcher.stop_requests == []


def test_nothing_due_is_an_empty_final_page() -> None:
    world = make_world()

    assert _body(_batch(world)) == {
        "protocol_version": "v1",
        "reconciled": [],
        "next_after": None,
    }


def test_a_due_run_is_reconciled_and_the_reported_state_is_already_stored(
    world: World,
) -> None:
    run_id = _past_deadline(world)

    body = _body(_batch(world))

    assert body["next_after"] is None
    assert [(e["run_id"], e["outcome"], e["state"]) for e in body["reconciled"]] == [
        (run_id, "reconciled", RunState.CANCEL_REQUESTED.value)
    ]
    assert world.store.get_run(run_id).state is RunState.CANCEL_REQUESTED


def test_a_bounded_batch_pages_through_every_due_run_exactly_once(world: World) -> None:
    run_ids = sorted(admit_and_start(world)[0] for _ in range(5))

    seen: list[str] = []
    after: str | None = None
    pages = 0
    while True:
        params: dict[str, Any] = {"limit": 2}
        if after is not None:
            params["after"] = after
        body = _body(_batch(world, params=params))
        assert len(body["reconciled"]) <= 2
        seen.extend(entry["run_id"] for entry in body["reconciled"])
        pages += 1
        after = body["next_after"]
        if after is None:
            break

    assert pages == 3
    assert seen == run_ids


def test_a_run_whose_write_conflicts_is_reported_and_the_rest_of_the_batch_still_runs(
    world: World, monkeypatch: pytest.MonkeyPatch
) -> None:
    stale = _past_deadline(world)
    other, _, _ = admit_and_start(world)
    version_before = world.store.get_run(stale).version
    original = world.store.compare_and_set

    def conflict_for_stale(run: Any, **kwargs: Any) -> None:
        if run.run_id == stale:
            raise ProtocolError(
                ErrorCode.VERSION_CONFLICT,
                "stored version differs from the version read",
            )
        original(run, **kwargs)

    monkeypatch.setattr(world.store, "compare_and_set", conflict_for_stale)

    outcomes = _outcomes(_batch(world))

    assert outcomes[stale]["outcome"] == "conflict"
    assert outcomes[stale]["code"] == ErrorCode.VERSION_CONFLICT.value
    assert world.store.get_run(stale).version == version_before
    assert outcomes[other]["outcome"] == "reconciled"


def test_an_unconfirmed_stop_is_hinted_again_only_after_its_backoff(
    world: World,
) -> None:
    run_id = _past_deadline(world)
    assert _outcomes(_batch(world))[run_id]["state"] == RunState.CANCEL_REQUESTED.value
    # The cancel has now outlived its deadline: that liveness decision is never delayed by backoff.
    assert _outcomes(_batch(world))[run_id]["state"] == RunState.TIMED_OUT.value
    unsettled = world.store.get_run(run_id)
    assert unsettled.reserved_seconds > 0
    assert unsettled.next_reconcile_at == world.clock.now() + timedelta(
        seconds=reconcile_backoff_seconds(2, run_id)
    )
    requests_before = len(world.launcher.stop_requests)

    assert _body(_batch(world))["reconciled"] == []
    assert len(world.launcher.stop_requests) == requests_before

    world.clock.advance(seconds=reconcile_backoff_seconds(2, run_id))
    assert run_id in _outcomes(_batch(world))
    assert len(world.launcher.stop_requests) > requests_before


@pytest.mark.parametrize(
    "failure_flag,outcome,state",
    [
        ("fail_next", "launch_failed", RunState.DISPATCHING),
        ("ambiguous_next", "reconciled", RunState.RECONCILING),
    ],
)
def test_a_launcher_failure_does_not_skip_a_later_cancel_or_relaunch_the_attempt(
    failure_flag: str, outcome: str, state: RunState
) -> None:
    clock = ManualClock()
    store = InMemoryRunStore()
    objects = InMemoryObjectStore()
    launcher = RecordingLauncher()
    admissions = InMemoryAdmissionSwitch()
    ports = make_ports(
        store=store,
        objects=objects,
        launcher=launcher,
        clock=clock,
        admissions=admissions,
        run_ids=["run_" + "f" * 32, "run_" + "1" * 32],
    )
    world = World(
        TestClient(create_app(policy=make_policy(), ports=ports)),
        clock, store, objects, launcher, admissions,
    )
    cancelling = _past_deadline(world)
    launching, _, _ = admit_and_start(world)
    setattr(launcher, failure_flag, True)
    calls_before = launcher.calls

    outcomes = _outcomes(_batch(world))

    assert outcomes[launching]["outcome"] == outcome
    assert outcomes[launching]["state"] == state.value
    fenced = store.get_run(launching)
    assert fenced.state is state
    assert fenced.reserved_seconds > 0
    assert outcomes[cancelling]["state"] == RunState.CANCEL_REQUESTED.value
    assert launcher.stop_requested(store.get_run(cancelling))
    assert launcher.calls == calls_before + 1

    _body(_batch(world))

    assert launcher.calls == calls_before + 1
    assert store.get_run(launching).attempt == fenced.attempt
    assert store.get_run(launching).claim_id == fenced.claim_id


def test_a_storage_failure_still_aborts_the_batch(
    world: World, monkeypatch: pytest.MonkeyPatch
) -> None:
    admit_and_start(world)

    def storage_unavailable(name: str, data: bytes) -> None:
        raise RuntimeError("control storage unavailable")

    monkeypatch.setattr(world.objects, "create_only", storage_unavailable)

    with pytest.raises(RuntimeError, match="control storage unavailable"):
        _batch(world)

    assert world.launcher.calls == 0


@pytest.mark.parametrize(
    "params",
    [{"limit": "0"}, {"limit": "51"}, {"limit": "many"}, {"after": "not-a-run"}],
)
def test_a_malformed_page_request_is_refused_before_any_run_is_read(
    world: World, params: dict[str, Any]
) -> None:
    run_id = _past_deadline(world)
    before = world.store.get_run(run_id)

    response = _batch(world, params=params)

    assert response.status_code == 400
    assert response.json()["error"]["code"] == ErrorCode.INVALID_REQUEST.value
    assert world.store.get_run(run_id) == before
