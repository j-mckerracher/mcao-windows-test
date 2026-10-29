"""A successful platform execution without a valid final artifact fails as missing_result (DPES-815).

The worker reports collecting, then its execution ends. If no valid result object exists for the
current attempt, the run fails with missing_result and is not retryable: the platform finished, so
replaying the attempt would spend budget on work whose output the controller can never verify.
"""

import pytest

from flows import World, admit_and_start, reconcile, to_starting, write_status
from mao_cloud.protocol import FailureCode, ObjectKind, RETRYABLE, object_name
from mao_cloud.runner import plan_result


def _collecting(world: World) -> tuple[str, object]:
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1, phase="running")
    assert reconcile(world, run_id)["state"] == "running"
    write_status(world, bootstrap, seq=2, phase="collecting")
    assert reconcile(world, run_id)["state"] == "collecting"
    return run_id, bootstrap


def test_missing_result_is_a_non_retryable_failure_code() -> None:
    assert FailureCode.MISSING_RESULT == "missing_result"
    assert RETRYABLE[FailureCode.MISSING_RESULT] is False


def test_collecting_run_whose_execution_ends_with_no_result_fails_as_missing_result(
    world: World,
) -> None:
    run_id, _ = _collecting(world)
    world.launcher.confirm_stopped(world.store.get_run(run_id))

    failed = reconcile(world, run_id)

    assert failed["state"] == "failed"
    assert failed["failure"]["code"] == "missing_result"
    assert failed["failure"]["retryable"] is False


def test_collecting_run_with_a_live_execution_keeps_waiting(world: World) -> None:
    run_id, _ = _collecting(world)

    assert reconcile(world, run_id)["state"] == "collecting"


def test_unreadable_result_after_the_execution_ends_fails_as_missing_result(
    world: World,
) -> None:
    run_id, bootstrap = _collecting(world)
    world.objects.write(
        object_name(ObjectKind.RESULT, run_id=run_id, attempt=bootstrap.attempt),
        b"{not a result document",
    )
    world.launcher.confirm_stopped(world.store.get_run(run_id))

    failed = reconcile(world, run_id)

    assert failed["state"] == "failed"
    assert failed["failure"]["code"] == "missing_result"


def test_valid_result_written_before_the_execution_ends_still_succeeds(world: World) -> None:
    run_id, bootstrap = _collecting(world)
    world.objects.put_planned(
        plan_result(bootstrap, outcome="succeeded", exit_code=0, commit_sha="b" * 40, artifacts=[])
    )
    world.launcher.confirm_stopped(world.store.get_run(run_id))

    assert reconcile(world, run_id)["state"] == "succeeded"


@pytest.mark.parametrize("outcome", ["succeeded", "failed"])
def test_result_arriving_during_execution_stop_observation_is_ingested(
    world: World, monkeypatch, outcome: str
) -> None:
    run_id, bootstrap = _collecting(world)
    world.launcher.confirm_stopped(world.store.get_run(run_id))
    observe = world.launcher.execution_stopped
    published = False

    def stop_after_upload(record) -> bool:
        nonlocal published
        if not published:
            published = True
            world.objects.put_planned(
                plan_result(
                    bootstrap, outcome=outcome, exit_code=0 if outcome == "succeeded" else 1,
                    commit_sha="b" * 40 if outcome == "succeeded" else None, artifacts=[],
                )
            )
        return observe(record)

    monkeypatch.setattr(world.launcher, "execution_stopped", stop_after_upload)
    result = reconcile(world, run_id)
    assert published
    assert result["state"] == outcome
    if outcome == "failed":
        assert result["failure"]["code"] == "worker_failed"
    else:
        assert result["failure"] is None
