"""The platform execution name of a launched attempt (DPES-809).

The name jobs.run returns is stored on the attempt by compare-and-set, right after the launch. It is
internal: no public view carries it. A retry clears it with the rest of the attempt. A stop or a
confirmation addresses only that name, and capacity is released only once the attempt is terminal
and the launcher confirms its execution is stopped. RecordingLauncher stands in for the platform;
its execution_name is the name each launch returns.
"""

import json
import logging
from datetime import timedelta
from typing import Any

import pytest
from fakes import EPOCH
from flows import (
    LONG_BUDGET,
    admit_and_start,
    dispatch,
    make_world,
    post_retry,
    reconcile,
    retry_body,
    view,
)
from mao_cloud.protocol import RunState

NAME = (
    "projects/mao-dev/locations/us-central1/jobs/mao-worker/executions/mao-worker-7f2kq"
)
OTHER_NAME = (
    "projects/mao-dev/locations/us-central1/jobs/mao-worker/executions/mao-worker-9x4pd"
)
RETRY_NAME = (
    "projects/mao-dev/locations/us-central1/jobs/mao-worker/executions/mao-worker-2hz8w"
)


def test_the_name_the_launch_returns_is_stored_on_the_dispatched_attempt() -> None:
    world = make_world()
    world.launcher.execution_name = NAME
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)

    dispatch(world, run_id)

    stored = world.store.get_run(run_id)
    assert stored is not None
    assert stored.state is RunState.DISPATCHING
    assert stored.execution_name == NAME


def test_the_execution_name_never_reaches_a_public_view() -> None:
    world = make_world()
    world.launcher.execution_name = NAME
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    dispatch(world, run_id)

    body = json.dumps(view(world, run_id))

    assert NAME not in body
    assert "execution_name" not in body


def test_a_launch_that_names_no_execution_stores_none_and_logs_an_error(
    caplog: pytest.LogCaptureFixture,
) -> None:
    world = make_world()
    world.launcher.execution_name = None
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)

    with caplog.at_level(logging.ERROR, logger="mao_cloud.audit"):
        dispatch(world, run_id)

    stored = world.store.get_run(run_id)
    assert stored is not None
    assert stored.state is RunState.DISPATCHING
    assert stored.execution_name is None
    assert "launch answer named no execution" in caplog.text


def test_a_conflicting_write_is_retried_and_the_concurrent_change_is_kept(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    world = make_world()
    world.launcher.execution_name = NAME
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    store = world.store
    real_compare_and_set = store.compare_and_set
    raced: list[int] = []

    def racing_compare_and_set(run: Any, **kwargs: Any) -> None:
        if run.execution_name is not None and not raced:
            # A concurrent writer lands first and moves the version, so this write is stale.
            raced.append(run.version)
            current = store.get_run(run_id)
            assert current is not None
            real_compare_and_set(
                current.model_copy(
                    update={
                        "version": current.version + 1,
                        "unknown_launch_alerted_at": EPOCH,
                    }
                ),
                expected_version=current.version,
            )
        return real_compare_and_set(run, **kwargs)

    monkeypatch.setattr(store, "compare_and_set", racing_compare_and_set)

    dispatch(world, run_id)

    stored = store.get_run(run_id)
    assert stored is not None
    assert len(raced) == 1
    assert stored.execution_name == NAME
    assert stored.unknown_launch_alerted_at == EPOCH
    assert stored.state is RunState.DISPATCHING


def test_a_name_already_stored_by_another_writer_is_never_overwritten(
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
) -> None:
    world = make_world()
    world.launcher.execution_name = NAME
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    store = world.store
    real_compare_and_set = store.compare_and_set
    raced: list[int] = []

    def racing_compare_and_set(run: Any, **kwargs: Any) -> None:
        if run.execution_name is not None and not raced:
            raced.append(run.version)
            current = store.get_run(run_id)
            assert current is not None
            real_compare_and_set(
                current.model_copy(
                    update={
                        "version": current.version + 1,
                        "execution_name": OTHER_NAME,
                    }
                ),
                expected_version=current.version,
            )
        return real_compare_and_set(run, **kwargs)

    monkeypatch.setattr(store, "compare_and_set", racing_compare_and_set)

    with caplog.at_level(logging.ERROR, logger="mao_cloud.audit"):
        dispatch(world, run_id)

    stored = store.get_run(run_id)
    assert stored is not None
    assert stored.execution_name == OTHER_NAME
    assert "execution name not recorded" in caplog.text


def test_a_retry_clears_the_stored_name_and_the_next_launch_stores_its_own(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    world = make_world()
    world.launcher.execution_name = NAME
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    dispatch(world, run_id)
    world.clock.advance(seconds=601)
    assert reconcile(world, run_id)["state"] == "reconciling"
    world.clock.advance(seconds=121)
    failed = reconcile(world, run_id)
    assert failed["state"] == "failed"
    assert failed["failure"]["code"] == "launch_uncertain"
    world.launcher.confirm_stopped(world.store.get_run(run_id))
    reconcile(world, run_id)

    retried = post_retry(
        world,
        run_id,
        retry_body(world, expected_version=view(world, run_id)["version"]),
    )

    assert retried.status_code == 200, retried.text
    queued = world.store.get_run(run_id)
    assert queued is not None
    assert queued.attempt == 2
    assert queued.execution_name is None

    world.launcher.execution_name = RETRY_NAME
    dispatch(world, run_id)

    relaunched = world.store.get_run(run_id)
    assert relaunched is not None
    assert relaunched.attempt == 2
    assert relaunched.execution_name == RETRY_NAME


def test_a_failed_attempt_with_a_stored_name_holds_capacity_until_its_stop_is_confirmed() -> (
    None
):
    world = make_world()
    world.launcher.execution_name = NAME
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    dispatch(world, run_id)
    world.clock.advance(seconds=601)
    assert reconcile(world, run_id)["state"] == "reconciling"
    world.clock.advance(seconds=121)
    assert reconcile(world, run_id)["state"] == "failed"

    held = world.store.get_run(run_id)
    assert held is not None
    assert held.reserved_seconds > 0
    assert world.launcher.stop_requested(held)

    world.launcher.confirm_stopped(held)
    reconcile(world, run_id)

    released = world.store.get_run(run_id)
    assert released is not None
    assert released.reserved_seconds == 0
    assert released.execution_name == NAME


def test_the_ten_minute_alert_is_recorded_while_the_stored_name_holds_capacity() -> (
    None
):
    world = make_world()
    world.launcher.execution_name = NAME
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    dispatch(world, run_id)

    world.clock.advance(seconds=601)
    reconcile(world, run_id)

    alerted = world.store.get_run(run_id)
    assert alerted is not None
    assert alerted.unknown_launch_alerted_at is not None
    assert alerted.reserved_seconds > 0
    assert alerted.dispatched_at is not None
    assert alerted.unknown_launch_alerted_at - alerted.dispatched_at >= timedelta(
        minutes=10
    )
