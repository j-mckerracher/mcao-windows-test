"""DPES-809 through the HTTP API: queue admission, owner round-robin dispatch, reserved capacity,
control documents, ambiguous launches, duplicate reconcile races, and bootstrap verification.

Everything runs against the in-memory fakes. No cloud is contacted.
"""

from typing import Any

import pytest

import fakes
from fakes import (
    CONTROLLER_HEADERS,
    OWNER_A,
    OWNER_B,
    SOURCE_TAR,
    admission_limits,
    create_payload,
    make_policy,
    make_ports,
    owner_headers,
    start_payload,
)
from flows import (
    LONG_BUDGET,
    admit_and_start,
    dispatch,
    fail_launch_uncertain,
    make_world,
    post_retry,
    retry_body,
    reconcile,
    view,
    World,
)
from mao_cloud import broker
from mao_cloud.signer import RefusalCode, SignerRefused
from mao_cloud.protocol import (
    ControlDocument,
    ObjectKind,
    RunState,
    WorkerIntake,
    build_bootstrap,
    object_name,
)

CANCEL_BODY = {"protocol_version": "v1"}
RESERVED = LONG_BUDGET["max_runtime_seconds"]


def _admit(world: World, owner: str) -> str:
    """Admit and start one run for owner. Each admission gets its own request id and instant."""
    world.clock.advance(seconds=1)
    run_id, _, _ = admit_and_start(world, owner, at=world.clock.now(), budget=LONG_BUDGET)
    return run_id


def _launch_order(world: World, run_ids: list[str]) -> list[str]:
    """Reconcile every run, repeatedly, in reverse order until nothing else launches.

    All launches share the same timestamp; persistent dispatch order must break ties.
    """
    order: list[str] = []
    progressed = True
    while progressed:
        progressed = False
        for run_id in reversed(run_ids):
            before = len(world.launcher.launched)
            reconcile(world, run_id)
            if len(world.launcher.launched) > before:
                order.append(world.launcher.launched[-1].run_id)
                progressed = True
    return order


# -- queue admission -----------------------------------------------------------


def test_eleventh_queued_start_for_one_owner_is_refused_while_another_owner_admits() -> None:
    world = make_world()
    for _ in range(10):
        _admit(world, OWNER_A)

    world.clock.advance(seconds=1)
    create = create_payload(at=world.clock.now(), budget=LONG_BUDGET)
    created = world.client.post("/v1/runs", json=create, headers=owner_headers(OWNER_A))
    assert created.status_code == 201, created.text
    run_id = created.json()["run"]["run_id"]
    name = created.json()["upload"]["object_name"]
    world.objects.write(name, SOURCE_TAR)

    refused = world.client.post(
        f"/v1/runs/{run_id}:start",
        json=start_payload(create, generation=world.objects.generation_of(name)),
        headers=owner_headers(OWNER_A),
    )
    assert refused.status_code == 429, refused.text
    assert refused.json()["error"]["code"] == "queue_full"

    _admit(world, OWNER_B)


# -- owner round-robin dispatch ------------------------------------------------


def test_dispatch_alternates_owners_and_holds_the_fifth_run_back() -> None:
    world = make_world()
    a1 = _admit(world, OWNER_A)
    a2 = _admit(world, OWNER_A)
    a3 = _admit(world, OWNER_A)
    a4 = _admit(world, OWNER_A)
    b1 = _admit(world, OWNER_B)

    order = _launch_order(world, [a1, a2, a3, a4, b1])

    assert order == [a1, b1, a2, a3]
    assert view(world, a4)["state"] == "queued"


def test_cancelling_a_dispatching_run_frees_its_slot_for_the_next_queued_run() -> None:
    world = make_world()
    a1 = _admit(world, OWNER_A)
    a2 = _admit(world, OWNER_A)
    a3 = _admit(world, OWNER_A)
    a4 = _admit(world, OWNER_A)
    b1 = _admit(world, OWNER_B)
    assert _launch_order(world, [a1, a2, a3, a4, b1]) == [a1, b1, a2, a3]

    cancelled = world.client.post(
        f"/v1/runs/{a1}:cancel", json=CANCEL_BODY, headers=owner_headers(OWNER_A)
    )
    assert cancelled.status_code == 200, cancelled.text
    assert cancelled.json()["state"] == "cancel_requested"

    # Until the cancelled execution is confirmed stopped, the run stays open and keeps its slot.
    assert reconcile(world, a1)["state"] == "cancel_requested"
    assert reconcile(world, a4)["state"] == "queued"

    world.launcher.confirm_stopped(world.store.get_run(a1))
    assert reconcile(world, a1)["state"] == "cancelled"
    bootstrap = dispatch(world, a4)
    assert bootstrap.run_id == a4


# -- reserved capacity and control document before launch -----------------------


def test_reservation_and_control_document_exist_before_the_launch_call(monkeypatch) -> None:
    world = make_world()
    run_id = _admit(world, OWNER_A)
    seen: dict[str, Any] = {}
    real_launch = world.launcher.launch

    def spy(bootstrap: Any, intake_url: str) -> None:
        record = world.store.get_run(run_id)
        seen["reserved"] = record.reserved_seconds
        seen["dispatched_at"] = record.dispatched_at
        seen["control"] = world.objects.get(
            object_name(ObjectKind.CONTROL, run_id=run_id, attempt=record.attempt)
        )
        seen["intake"] = world.objects.get(
            object_name(ObjectKind.INTAKE, run_id=run_id, attempt=record.attempt)
        )
        seen["intake_url"] = intake_url
        real_launch(bootstrap, intake_url)

    monkeypatch.setattr(world.launcher, "launch", spy)
    bootstrap = dispatch(world, run_id)

    assert seen["reserved"] == RESERVED
    assert seen["dispatched_at"] == world.clock.now()
    assert seen["control"] is not None
    control = ControlDocument.model_validate_json(seen["control"])
    assert (control.run_id, control.attempt, control.fence, control.claim_id) == (
        bootstrap.run_id,
        bootstrap.attempt,
        bootstrap.fence,
        bootstrap.claim_id,
    )


def test_the_intake_exists_and_is_signed_before_the_launch_call(monkeypatch) -> None:
    world = make_world()
    run_id = _admit(world, OWNER_A)
    seen: dict[str, Any] = {}
    real_launch = world.launcher.launch

    def spy(bootstrap: Any, intake_url: str) -> None:
        record = world.store.get_run(run_id)
        seen["intake"] = world.objects.get(
            object_name(ObjectKind.INTAKE, run_id=run_id, attempt=record.attempt)
        )
        seen["intake_url"] = intake_url
        real_launch(bootstrap, intake_url)

    monkeypatch.setattr(world.launcher, "launch", spy)
    bootstrap = dispatch(world, run_id)

    assert seen["intake"] is not None
    intake = WorkerIntake.model_validate_json(seen["intake"])
    # The intake names exactly the bootstrap the launch carries, and the accepted receipt.
    assert intake.bootstrap == bootstrap
    assert intake.receipt.run_id == run_id
    # The URL is the intake object's own signed capability. It is a credential, so it is asserted
    # by containment and never printed.
    assert object_name(ObjectKind.INTAKE, run_id=run_id, attempt=bootstrap.attempt) in seen[
        "intake_url"
    ]


def test_a_refused_intake_capability_keeps_jobs_run_uncalled(monkeypatch) -> None:
    real_make_signer = fakes.make_signer

    class RefusingIntake:
        """The real signer for every capability except the intake read, which is refused."""

        def __init__(self, inner: Any) -> None:
            self._inner = inner

        def issue(self, request: Any) -> Any:
            if request.kind is ObjectKind.INTAKE:
                raise SignerRefused(RefusalCode.BLOCKED)
            return self._inner.issue(request)

    monkeypatch.setattr(
        fakes,
        "make_signer",
        lambda store, clock: RefusingIntake(real_make_signer(store, clock)),
    )
    world = make_world()
    run_id = _admit(world, OWNER_A)

    with pytest.raises(broker.LaunchFailed, match="intake capability was not issued"):
        world.client.post(f"/internal/v1/runs/{run_id}:reconcile", headers=CONTROLLER_HEADERS)

    # The attempt keeps its reservation and is never relaunched; jobs.run was never called.
    assert world.store.get_run(run_id).state is RunState.DISPATCHING
    assert world.launcher.launched == []
    assert world.launcher.calls == 0


# -- ambiguous launch ----------------------------------------------------------


def test_ambiguous_launch_keeps_capacity_until_execution_is_confirmed_stopped() -> None:
    world = make_world()
    run_id = _admit(world, OWNER_A)
    world.launcher.ambiguous_next = True

    assert reconcile(world, run_id)["state"] == "reconciling"
    record = world.store.get_run(run_id)
    assert record.reserved_seconds == RESERVED
    assert world.launcher.launched == []
    assert world.launcher.calls == 1

    # Repeated reconcile while reconciling never relaunches the attempt.
    assert reconcile(world, run_id)["state"] == "reconciling"
    assert world.launcher.calls == 1

    world.clock.advance(seconds=121)
    failed = reconcile(world, run_id)
    assert failed["state"] == "failed"
    assert failed["failure"]["code"] == "launch_uncertain"
    record = world.store.get_run(run_id)
    assert (record.consumed_seconds, record.reserved_seconds) == (0, RESERVED)
    refused = post_retry(world, run_id, retry_body(world, expected_version=record.version))
    assert refused.status_code == 409
    assert refused.json()["error"]["code"] == "retry_not_allowed"
    world.launcher.confirm_stopped(record)
    reconcile(world, run_id)
    settled = world.store.get_run(run_id)
    assert (settled.consumed_seconds, settled.reserved_seconds) == (121, 0)
    reconcile(world, run_id)
    assert world.store.get_run(run_id) == settled
    assert world.launcher.calls == 1


# -- duplicate reconcile race --------------------------------------------------


def test_stale_queued_snapshot_cannot_launch_the_attempt_a_second_time(monkeypatch) -> None:
    world = make_world()
    run_id = _admit(world, OWNER_A)
    stale = world.store.get_run(run_id)
    stale_capacity = world.store.admission_snapshot()
    dispatch(world, run_id)
    assert world.launcher.calls == 1

    # The racing reconcile read the run while it was still queued, and selected it from that read.
    # Its dispatch commit must lose the version check, and the durable record must win.
    durable_get = world.store.get_run
    reads: list[str] = []

    def first_read_is_stale(read_run_id: str) -> Any:
        reads.append(read_run_id)
        return stale if len(reads) == 1 else durable_get(read_run_id)

    monkeypatch.setattr(world.store, "get_run", first_read_is_stale)
    monkeypatch.setattr(world.store, "admission_snapshot", lambda: stale_capacity)
    response = world.client.post(
        f"/internal/v1/runs/{run_id}:reconcile", headers=CONTROLLER_HEADERS
    )

    assert response.status_code == 200, response.text
    assert response.json()["state"] == "dispatching"
    assert world.launcher.calls == 1
    assert len(world.launcher.launched) == 1


# -- bootstrap verification ----------------------------------------------------


def test_tampered_bootstrap_is_refused_before_any_launch(monkeypatch) -> None:
    world = make_world()
    run_id = _admit(world, OWNER_A)

    def tampered(record: Any) -> Any:
        bootstrap = build_bootstrap(record)
        source = bootstrap.source.model_copy(update={"generation": bootstrap.source.generation + 1})
        return bootstrap.model_copy(update={"source": source})

    monkeypatch.setattr(broker, "build_bootstrap", tampered)
    response = world.client.post(
        f"/internal/v1/runs/{run_id}:reconcile", headers=CONTROLLER_HEADERS
    )

    assert response.status_code == 409, response.text
    assert response.json()["error"]["code"] == "bootstrap_mismatch"
    assert world.launcher.calls == 0
    assert world.launcher.launched == []


def _direct_broker(world: World) -> broker.Broker:
    return broker.Broker(
        policy=make_policy(),
        ports=make_ports(
            store=world.store, objects=world.objects,
            launcher=world.launcher, clock=world.clock,
        ),
    )


def _uploaded(world: World, owner: str) -> tuple[str, dict[str, Any]]:
    create = create_payload(at=world.clock.now(), budget=LONG_BUDGET)
    response = world.client.post("/v1/runs", json=create, headers=owner_headers(owner))
    assert response.status_code == 201
    run_id = response.json()["run"]["run_id"]
    name = object_name(ObjectKind.INPUT, run_id=run_id)
    generation = world.objects.write(name, SOURCE_TAR)
    return run_id, start_payload(create, generation=generation)


def test_concurrent_starts_for_distinct_runs_cannot_overfill_owner_queue(monkeypatch) -> None:
    world = make_world(policy=make_policy(admission=admission_limits(owner_queue_depth=2)))
    _admit(world, OWNER_A)
    first, first_body = _uploaded(world, OWNER_A)
    second, second_body = _uploaded(world, OWNER_A)
    other = _direct_broker(world)
    real_snapshot = world.store.admission_snapshot

    def interleave() -> Any:
        capacity = real_snapshot()
        monkeypatch.setattr(world.store, "admission_snapshot", real_snapshot)
        other.start_run(OWNER_A, second, second_body)
        return capacity

    monkeypatch.setattr(world.store, "admission_snapshot", interleave)
    refused = world.client.post(
        f"/v1/runs/{first}:start", json=first_body, headers=owner_headers(OWNER_A)
    )
    assert refused.status_code == 409
    assert refused.json()["error"]["code"] == "version_conflict"
    assert world.store.get_run(first).state is RunState.AWAITING_UPLOAD
    assert sum(r.state is RunState.QUEUED for r in world.store.list_runs()) == 2


def test_retry_and_distinct_start_share_the_same_queue_admission_guard(monkeypatch) -> None:
    world = make_world(policy=make_policy(admission=admission_limits(owner_queue_depth=2)))
    failed, _ = fail_launch_uncertain(world)
    _admit(world, OWNER_A)
    second, second_body = _uploaded(world, OWNER_A)
    before = world.store.get_run(failed)
    body = retry_body(world, expected_version=before.version)
    other = _direct_broker(world)
    real_snapshot = world.store.admission_snapshot

    def interleave() -> Any:
        capacity = real_snapshot()
        monkeypatch.setattr(world.store, "admission_snapshot", real_snapshot)
        other.start_run(OWNER_A, second, second_body)
        return capacity

    monkeypatch.setattr(world.store, "admission_snapshot", interleave)
    refused = post_retry(world, failed, body)
    assert refused.status_code == 409
    assert refused.json()["error"]["code"] == "version_conflict"
    assert world.store.get_run(failed) == before
    assert sum(r.state is RunState.QUEUED for r in world.store.list_runs()) == 2


@pytest.mark.parametrize("holders", [2, 3])
def test_racing_dispatch_for_a_distinct_run_cannot_overbook_slots_or_budget(
    monkeypatch, holders: int
) -> None:
    daily_cap = (holders + 1) * RESERVED
    world = make_world(
        policy=make_policy(admission=admission_limits(team_daily_worker_seconds=daily_cap))
    )
    for _ in range(holders):
        dispatch(world, _admit(world, OWNER_A))
    waiting = _admit(world, OWNER_A)
    real_snapshot = world.store.admission_snapshot
    other = _direct_broker(world)
    competitor: list[str] = []

    def interleave() -> Any:
        capacity = real_snapshot()
        monkeypatch.setattr(world.store, "admission_snapshot", real_snapshot)
        fresh = _admit(world, OWNER_B)
        competitor.append(fresh)
        other.reconcile(fresh)
        return capacity

    monkeypatch.setattr(world.store, "admission_snapshot", interleave)
    assert reconcile(world, waiting)["state"] == "queued"
    assert world.store.get_run(competitor[0]).state is RunState.DISPATCHING
    assert len(world.launcher.launched) == holders + 1
    assert sum(r.reserved_seconds for r in world.store.list_runs()) == (holders + 1) * RESERVED


def test_same_timestamp_dispatches_keep_alternating_owners() -> None:
    world = make_world()
    a1, a2, a3 = [_admit(world, OWNER_A) for _ in range(3)]
    b1, b2 = [_admit(world, OWNER_B) for _ in range(2)]
    assert _launch_order(world, [a1, a2, a3, b1, b2]) == [a1, b1, a2, b2]
    records = [world.store.get_run(run_id) for run_id in [a1, b1, a2, b2]]
    assert len({r.dispatched_at for r in records}) == 1
    assert [r.dispatch_order for r in records] == sorted({r.dispatch_order for r in records})


def test_ambiguous_execution_keeps_fifth_run_queued_past_stale_and_deadline() -> None:
    world = make_world()
    unknown = _admit(world, OWNER_A)
    world.launcher.ambiguous_next = True
    reconcile(world, unknown)
    for _ in range(3):
        dispatch(world, _admit(world, OWNER_B))
    waiting = _admit(world, OWNER_A)
    world.clock.advance(seconds=121)
    assert reconcile(world, unknown)["state"] == "failed"
    assert reconcile(world, waiting)["state"] == "queued"
    world.clock.advance(seconds=3600)
    reconcile(world, unknown)
    held = world.store.get_run(unknown)
    assert held.reserved_seconds == RESERVED
    assert held.consumed_seconds == 0
    world.launcher.confirm_stopped(held)
    reconcile(world, unknown)
    assert world.store.get_run(unknown).reserved_seconds == 0
