"""Bounded, backed-off reconcile scheduling (DPES-809 AC6).

Planning is pure. A planned page returns at most its limit of actions, skipping runs that need
none, and its cursor walks every run exactly once. Backoff delays only the planner's reconcile
hints, so an unsettled
execution is not asked to stop on every tick. Liveness decisions are never delayed, and only the
launch adapter's confirmation releases a reservation.
"""

from datetime import timedelta
from typing import Any

from fakes import make_policy
from flows import LONG_BUDGET, World, admit_and_start, dispatch, reconcile
from mao_cloud.protocol import RunRecord, RunState
from mao_cloud.sweeper import (
    RECONCILE_BACKOFF_CAP_SECONDS,
    plan_sweep_page,
    reconcile_backoff_seconds,
)


def _with(template: RunRecord, **fields: Any) -> RunRecord:
    return template.model_copy(update=fields)


def _template(world: World) -> RunRecord:
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    return world.store.get_run(run_id)


def test_backoff_stays_between_the_jittered_floor_and_the_nominal_step_up_to_the_cap() -> None:
    for attempts in range(1, 21):
        nominal = min(RECONCILE_BACKOFF_CAP_SECONDS, 30 * 2 ** (attempts - 1))
        delay = reconcile_backoff_seconds(attempts, "run-a")
        assert nominal * 3 // 4 <= delay <= nominal
    assert reconcile_backoff_seconds(20, "run-a") <= RECONCILE_BACKOFF_CAP_SECONDS


def test_backoff_is_deterministic_per_run_and_spreads_across_runs() -> None:
    assert reconcile_backoff_seconds(5, "run-a") == reconcile_backoff_seconds(5, "run-a")
    delays = {reconcile_backoff_seconds(5, f"run-{i:03}") for i in range(64)}
    assert len(delays) > 1


def test_a_page_is_bounded_and_its_cursor_visits_every_record_exactly_once(world: World) -> None:
    template = _template(world)
    records = [
        _with(template, run_id=f"run-{i:03}", state=RunState.FAILED, reserved_seconds=60)
        for i in range(7)
    ]
    now = world.clock.now()
    policy = make_policy()

    seen: list[str] = []
    after: str | None = None
    pages = 0
    while True:
        page = plan_sweep_page(list(reversed(records)), now=now, policy=policy, limit=3, after=after)
        assert len(page.actions) <= 3
        seen.extend(action.run_id for action in page.actions)
        pages += 1
        after = page.next_after
        if after is None:
            break
    assert pages == 3
    assert seen == [f"run-{i:03}" for i in range(7)]


def test_a_cursor_past_the_last_record_is_empty_and_final(world: World) -> None:
    template = _template(world)
    page = plan_sweep_page(
        [_with(template, run_id="run-001", state=RunState.FAILED, reserved_seconds=60)],
        now=world.clock.now(),
        policy=make_policy(),
        limit=3,
        after="run-zzz",
    )
    assert page.actions == []
    assert page.next_after is None


def test_a_page_limit_counts_actions_so_a_caller_that_drops_the_cursor_still_reaches_due_runs(
    world: World,
) -> None:
    template = _template(world)
    idle = [
        _with(template, run_id=f"run-{i:03}", state=RunState.SUCCEEDED, reserved_seconds=0)
        for i in range(3)
    ]
    due = [
        _with(template, run_id=f"run-{i:03}", state=RunState.FAILED, reserved_seconds=60)
        for i in (3, 4)
    ]

    page = plan_sweep_page(
        [*idle, *due], now=world.clock.now(), policy=make_policy(), limit=1, after=None
    )

    assert [action.run_id for action in page.actions] == ["run-003"]
    assert page.next_after == "run-003"


def test_a_cursor_walk_over_mixed_runs_visits_each_due_run_exactly_once(world: World) -> None:
    template = _template(world)
    records = [
        _with(template, run_id=f"run-{i:03}", state=RunState.SUCCEEDED, reserved_seconds=0)
        if i % 2 == 0
        else _with(template, run_id=f"run-{i:03}", state=RunState.FAILED, reserved_seconds=60)
        for i in range(9)
    ]

    seen: list[str] = []
    after: str | None = None
    while True:
        page = plan_sweep_page(
            records, now=world.clock.now(), policy=make_policy(), limit=2, after=after
        )
        assert len(page.actions) <= 2
        seen.extend(action.run_id for action in page.actions)
        after = page.next_after
        if after is None:
            break
    assert seen == ["run-001", "run-003", "run-005", "run-007"]


def test_a_page_that_fills_on_its_last_action_is_final(world: World) -> None:
    template = _template(world)
    page = plan_sweep_page(
        [_with(template, run_id="run-001", state=RunState.FAILED, reserved_seconds=60)],
        now=world.clock.now(),
        policy=make_policy(),
        limit=1,
        after=None,
    )
    assert [action.run_id for action in page.actions] == ["run-001"]
    assert page.next_after is None


def test_backoff_delays_the_reconcile_hint_but_never_a_liveness_decision(world: World) -> None:
    template = _template(world)
    now = world.clock.now()
    policy = make_policy()
    waiting = _with(
        template,
        run_id="run-wait",
        state=RunState.FAILED,
        reserved_seconds=60,
        next_reconcile_at=now + timedelta(seconds=100),
    )

    early = plan_sweep_page([waiting], now=now, policy=policy, limit=10, after=None)
    assert early.actions == []

    due = plan_sweep_page(
        [waiting], now=now + timedelta(seconds=100), policy=policy, limit=10, after=None
    )
    assert [action.run_id for action in due.actions] == ["run-wait"]


def test_unconfirmed_stop_schedules_backoff_and_explicit_reconcile_still_requests_the_stop(
    world: World,
) -> None:
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    dispatch(world, run_id)
    world.clock.advance(seconds=3700)
    assert reconcile(world, run_id)["state"] == "cancel_requested"
    scheduled = world.store.get_run(run_id)
    assert scheduled.reserved_seconds > 0
    assert scheduled.unsettled_reconciles == 1
    assert scheduled.next_reconcile_at == world.clock.now() + timedelta(
        seconds=reconcile_backoff_seconds(1, run_id)
    )

    requests_before = len(world.launcher.stop_requests)
    reconcile(world, run_id)
    assert len(world.launcher.stop_requests) > requests_before
    assert world.store.get_run(run_id).unsettled_reconciles == 2


def test_confirmed_absence_clears_the_backoff_schedule(world: World) -> None:
    run_id, _, _ = admit_and_start(world, budget=LONG_BUDGET)
    dispatch(world, run_id)
    world.clock.advance(seconds=3700)
    reconcile(world, run_id)

    world.launcher.confirm_stopped(world.store.get_run(run_id))
    reconcile(world, run_id)
    settled = world.store.get_run(run_id)
    assert settled.reserved_seconds == 0
    assert settled.unsettled_reconciles == 0
    assert settled.next_reconcile_at is None
