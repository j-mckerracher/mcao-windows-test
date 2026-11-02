"""Following a run's logs: stable cursors, bounded pages, capped backoff, redaction (DPES-815 AC3).

The follow engine is driven against the real HTTP app through TestClient. Waits are recorded
instead of slept, so backoff is asserted exactly. Nothing here reaches a cloud.
"""

from collections.abc import Callable
from typing import Any

import pytest
from fakes import OWNER_A, owner_headers
from flows import World, admit_and_start, reconcile, to_starting, write_status
from mao_cloud.log_follow import (
    Backoff,
    EndEvent,
    EntryEvent,
    GapEvent,
    TransientFetchError,
    follow_logs,
)
from mao_cloud.protocol import (
    MAX_LOG_LIMIT,
    ErrorCode,
    LogPage,
    ObjectKind,
    ProtocolError,
    RunState,
    object_name,
)
from mao_cloud.runner import plan_log_entry, plan_result

SECRET = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ3b3JrZXIifQ.c2lnbmF0dXJl"  # gitleaks:allow - synthetic JWT-shaped redaction fixture, not a credential


class _Stop(Exception):
    """Raised by a recording sleep to end an otherwise endless follow."""


def _running(world: World) -> tuple[str, Any]:
    run_id, _, receipt = admit_and_start(world)
    bootstrap = to_starting(world, run_id, receipt)
    write_status(world, bootstrap, seq=1, phase="running")
    assert reconcile(world, run_id)["state"] == "running"
    return run_id, bootstrap


def _publish(
    world: World,
    run_id: str,
    bootstrap: Any,
    entries: dict[int, list[str]],
    *,
    status_seq: int,
    watermark: int,
) -> None:
    for seq, lines in entries.items():
        world.objects.put_planned(plan_log_entry(bootstrap, seq=seq, lines=lines))
    write_status(world, bootstrap, seq=status_seq, log_watermark=watermark, phase="running")
    assert reconcile(world, run_id)["state"] == "running"


def _finish(world: World, run_id: str, bootstrap: Any) -> None:
    world.objects.put_planned(
        plan_result(bootstrap, outcome="succeeded", exit_code=0, commit_sha="b" * 40, artifacts=[])
    )
    assert reconcile(world, run_id)["state"] == "succeeded"


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


def _app_fetch(world: World, run_id: str) -> Callable[[str | None, int], LogPage]:
    def fetch(cursor: str | None, limit: int) -> LogPage:
        params = {"limit": str(limit)}
        if cursor is not None:
            params["cursor"] = cursor
        response = world.client.get(
            f"/v1/runs/{run_id}/logs", params=params, headers=owner_headers(OWNER_A)
        )
        if response.status_code >= 500:
            raise TransientFetchError(response.text)
        assert response.status_code == 200, response.text
        return LogPage.model_validate(response.json())

    return fetch


def _no_sleep(seconds: float) -> None:
    raise AssertionError(f"follow slept {seconds}s where entries were already available")


def test_follow_yields_every_entry_in_order_then_ends_once_the_run_is_terminal(
    world: World,
) -> None:
    run_id, bootstrap = _running(world)
    _publish(
        world,
        run_id,
        bootstrap,
        {1: ["install ok"], 2: ["tests collected"], 3: ["all green"]},
        status_seq=2,
        watermark=3,
    )
    _finish(world, run_id, bootstrap)
    waits: list[float] = []

    events = list(follow_logs(_app_fetch(world, run_id), sleep=waits.append, limit=2))

    assert [type(e) for e in events] == [EntryEvent, EntryEvent, EntryEvent, EndEvent]
    assert [e.entry.seq for e in events[:3]] == [1, 2, 3]
    assert [e.entry.lines for e in events[:3]] == [
        ("install ok",),
        ("tests collected",),
        ("all green",),
    ]
    assert events[-1] == EndEvent(state=RunState.SUCCEEDED, watermark=3)
    assert waits == [], "pages with entries are fetched without waiting"


def test_empty_polls_back_off_exponentially_to_the_cap(world: World) -> None:
    run_id, _ = _running(world)
    waits: list[float] = []

    def sleep(seconds: float) -> None:
        waits.append(seconds)
        if len(waits) == 7:
            raise _Stop()

    with pytest.raises(_Stop):
        list(follow_logs(_app_fetch(world, run_id), sleep=sleep, backoff=Backoff()))

    assert waits == [1.0, 2.0, 4.0, 8.0, 16.0, 30.0, 30.0]


def test_backoff_restarts_from_the_initial_wait_once_entries_arrive(world: World) -> None:
    run_id, bootstrap = _running(world)
    real = _app_fetch(world, run_id)
    calls = 0
    waits: list[float] = []

    def fetch(cursor: str | None, limit: int) -> LogPage:
        nonlocal calls
        calls += 1
        if calls == 4:
            _publish(world, run_id, bootstrap, {1: ["ready"]}, status_seq=2, watermark=1)
        return real(cursor, limit)

    def sleep(seconds: float) -> None:
        waits.append(seconds)
        if len(waits) == 4:
            raise _Stop()

    with pytest.raises(_Stop):
        list(follow_logs(fetch, sleep=sleep))

    assert waits == [1.0, 2.0, 4.0, 1.0]


def test_transient_failures_are_retried_with_backoff_then_surface_past_the_limit(
    world: World,
) -> None:
    def always_down(cursor: str | None, limit: int) -> LogPage:
        raise TransientFetchError("connection reset")

    waits: list[float] = []

    with pytest.raises(TransientFetchError):
        list(follow_logs(always_down, sleep=waits.append, max_failures=3))

    assert waits == [1.0, 2.0, 4.0]


def test_progress_resumes_after_transient_failures_clear(world: World) -> None:
    run_id, bootstrap = _running(world)
    _publish(world, run_id, bootstrap, {1: ["one"]}, status_seq=2, watermark=1)
    _finish(world, run_id, bootstrap)
    real = _app_fetch(world, run_id)
    failures_left = 2

    def flaky(cursor: str | None, limit: int) -> LogPage:
        nonlocal failures_left
        if failures_left:
            failures_left -= 1
            raise TransientFetchError("503")
        return real(cursor, limit)

    waits: list[float] = []
    events = list(follow_logs(flaky, sleep=waits.append, max_failures=3))

    assert waits == [1.0, 2.0]
    assert [type(e) for e in events] == [EntryEvent, EndEvent]


def test_a_non_retryable_protocol_error_is_raised_without_waiting(world: World) -> None:
    def refused(cursor: str | None, limit: int) -> LogPage:
        raise ProtocolError(
            ErrorCode.CURSOR_INVALID, "cursor belongs to a previous attempt", attempt=1
        )

    waits: list[float] = []

    with pytest.raises(ProtocolError) as refusal:
        list(follow_logs(refused, sleep=waits.append))

    assert refusal.value.code is ErrorCode.CURSOR_INVALID
    assert waits == []


def test_a_replayed_page_after_a_lost_response_does_not_duplicate_entries(world: World) -> None:
    run_id, bootstrap = _running(world)
    _publish(world, run_id, bootstrap, {1: ["a"], 2: ["b"], 3: ["c"]}, status_seq=2, watermark=3)
    _finish(world, run_id, bootstrap)
    real = _app_fetch(world, run_id)
    remembered: list[LogPage] = []
    calls = 0

    def replaying(cursor: str | None, limit: int) -> LogPage:
        nonlocal calls
        calls += 1
        if calls == 1:
            page = real(cursor, limit)
            remembered.append(page)
            return page
        if calls == 2:
            return remembered[0]
        return real(cursor, limit)

    events = list(follow_logs(replaying, sleep=_no_sleep, limit=2))

    seqs = [e.entry.seq for e in events if isinstance(e, EntryEvent)]
    assert seqs == [1, 2, 3]
    assert isinstance(events[-1], EndEvent)


def test_a_cursor_into_trimmed_history_discloses_the_gap_once_then_follows_retained_entries(
    world: World,
) -> None:
    run_id, bootstrap = _running(world)
    _publish(
        world,
        run_id,
        bootstrap,
        {1: ["gone"], 2: ["gone too"], 3: ["kept"], 4: ["kept too"]},
        status_seq=2,
        watermark=4,
    )
    _finish(world, run_id, bootstrap)
    _trim(world, run_id, retained_from=3, deleted=(1, 2))

    events = list(follow_logs(_app_fetch(world, run_id), sleep=_no_sleep))

    gaps = [e for e in events if isinstance(e, GapEvent)]
    assert gaps == [GapEvent(from_seq=1, to_seq=2, retained_from=3, watermark=4)]
    assert [e.entry.seq for e in events if isinstance(e, EntryEvent)] == [3, 4]
    assert events[0] == gaps[0], "the gap is disclosed before the first retained entry"


def test_a_secret_in_a_log_line_never_reaches_a_followed_entry(world: World) -> None:
    run_id, bootstrap = _running(world)
    _publish(
        world,
        run_id,
        bootstrap,
        {1: [f"token {SECRET}", "tests passed"]},
        status_seq=2,
        watermark=1,
    )
    _finish(world, run_id, bootstrap)

    events = list(follow_logs(_app_fetch(world, run_id), sleep=_no_sleep))

    rendered = "\n".join(
        line for e in events if isinstance(e, EntryEvent) for line in e.entry.lines
    )
    assert SECRET not in rendered
    assert "tests passed" in rendered


@pytest.mark.parametrize("limit", [0, MAX_LOG_LIMIT + 1])
def test_a_limit_outside_the_protocol_bound_is_refused_before_any_fetch(limit: int) -> None:
    def never(cursor: str | None, limit: int) -> LogPage:
        raise AssertionError("no page may be fetched for an invalid limit")

    with pytest.raises(ValueError):
        follow_logs(never, sleep=_no_sleep, limit=limit)


def test_backoff_must_start_positive_and_cap_at_or_above_its_start() -> None:
    with pytest.raises(ValueError):
        Backoff(initial_seconds=0)
    with pytest.raises(ValueError):
        Backoff(initial_seconds=10, max_seconds=5)


def test_a_replayed_gap_only_page_is_reported_once_while_the_run_is_active(world: World) -> None:
    run_id, bootstrap = _running(world)
    _publish(
        world,
        run_id,
        bootstrap,
        {1: ["gone"], 2: ["gone too"], 3: ["also gone"]},
        status_seq=2,
        watermark=3,
    )
    _trim(world, run_id, retained_from=4, deleted=(1, 2, 3))
    real = _app_fetch(world, run_id)
    remembered: list[LogPage] = []
    calls = 0
    waits: list[float] = []

    def replaying(cursor: str | None, limit: int) -> LogPage:
        nonlocal calls
        calls += 1
        if calls == 1:
            page = real(cursor, limit)
            assert not page.entries
            assert page.snapshot.state is RunState.RUNNING
            remembered.append(page)
            return page
        if calls == 2:
            return remembered[0]
        if calls == 3:
            _publish(world, run_id, bootstrap, {4: ["new"]}, status_seq=3, watermark=4)
            _finish(world, run_id, bootstrap)
        return real(cursor, limit)

    events = list(follow_logs(replaying, sleep=waits.append))

    assert [event for event in events if isinstance(event, GapEvent)] == [
        GapEvent(from_seq=1, to_seq=3, retained_from=4, watermark=3)
    ]
    assert [event.entry.seq for event in events if isinstance(event, EntryEvent)] == [4]
    assert events[-1] == EndEvent(state=RunState.SUCCEEDED, watermark=4)
    assert waits == [1.0, 2.0]


def test_resuming_at_the_end_of_a_terminal_runs_watermark_ends_without_polling(
    world: World,
) -> None:
    run_id, bootstrap = _running(world)
    _publish(world, run_id, bootstrap, {1: ["one"], 2: ["two"]}, status_seq=2, watermark=2)
    _finish(world, run_id, bootstrap)
    real = _app_fetch(world, run_id)
    prior_page = real(None, MAX_LOG_LIMIT)
    assert prior_page.next_cursor is not None

    events = list(follow_logs(real, cursor=prior_page.next_cursor, sleep=_no_sleep))

    assert events == [EndEvent(state=RunState.SUCCEEDED, watermark=2)]


def test_fully_trimmed_terminal_history_discloses_the_gap_then_ends(world: World) -> None:
    run_id, bootstrap = _running(world)
    _publish(world, run_id, bootstrap, {1: ["one"], 2: ["two"]}, status_seq=2, watermark=2)
    _finish(world, run_id, bootstrap)
    _trim(world, run_id, retained_from=3, deleted=(1, 2))

    events = list(follow_logs(_app_fetch(world, run_id), sleep=_no_sleep))

    assert events == [
        GapEvent(from_seq=1, to_seq=2, retained_from=3, watermark=2),
        EndEvent(state=RunState.SUCCEEDED, watermark=2),
    ]
