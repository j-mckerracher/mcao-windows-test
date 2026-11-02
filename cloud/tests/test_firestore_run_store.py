"""FirestoreRunStore against the in-memory Firestore fake.

Parity cases run against both InMemoryRunStore and FirestoreRunStore. The rest pin the Firestore
layout: the shared revision, bounded admission reads, the owner dispatch index, layout provisioning,
transaction contention, and per-document write rates. Nothing here touches a real Firestore backend,
and nothing here is evidence about one. The fake models only what the adapter relies on.
"""

import ast
import uuid
from collections.abc import Callable
from datetime import timedelta
from pathlib import Path
from typing import Any

import pytest
from pydantic import AwareDatetime, BaseModel, ConfigDict, Field, ValidationError

from fakes import (
    EPOCH,
    OWNER_A,
    OWNER_B,
    InMemoryRunStore,
    ManualClock,
    admission_limits,
    run_record,
)
from firestore_fake import FakeFirestore
from mao_cloud import firestore_store
from mao_cloud.admission import select_next_dispatch
from mao_cloud.broker import RunStore
from mao_cloud.firestore_store import (
    ADMISSION_SHARDS,
    SHARDS,
    FirestoreRunStore,
    FirestoreStoreError,
    provision_admission_layout,
    shard_index,
)
from mao_cloud.protocol import (
    MAX_ATTEMPTS,
    AcceptedReceipt,
    ClaimId,
    CreateRunManifest,
    ErrorCode,
    IdempotencyRecord,
    ImageDigest,
    ProtocolError,
    RunId,
    RunRecord,
    RunState,
    Sha256Hex,
    TypedFailure,
)


class _PreviousRunRecord(BaseModel):
    """The strict stored-record reader before execution names were introduced."""

    model_config = ConfigDict(extra="forbid")

    run_id: RunId
    owner_id: str = Field(min_length=1)
    server_id: str = Field(min_length=1)
    request_uuid: uuid.UUID
    manifest: CreateRunManifest
    manifest_digest: Sha256Hex
    image_digest: ImageDigest
    state: RunState
    version: int = Field(ge=0)
    attempt: int = Field(ge=1)
    fence: int = Field(ge=1)
    max_attempts: int = Field(ge=1, le=MAX_ATTEMPTS)
    admitted_at: AwareDatetime
    state_entered_at: AwareDatetime
    deadline_at: AwareDatetime
    receipt: AcceptedReceipt | None = None
    claim_id: ClaimId | None = None
    retry_request_uuids: tuple[uuid.UUID, ...] = ()
    last_seen_at: AwareDatetime | None = None
    last_observed_at: AwareDatetime | None = None
    last_status_seq: int = Field(default=0, ge=0)
    last_log_watermark: int = Field(default=0, ge=0)
    log_retained_from: int = Field(default=1, ge=1)
    failure: TypedFailure | None = None
    dispatched_at: AwareDatetime | None = None
    unknown_launch_alerted_at: AwareDatetime | None = None
    unsettled_reconciles: int = Field(default=0, ge=0)
    next_reconcile_at: AwareDatetime | None = None
    dispatch_order: int = Field(default=0, ge=0)
    reserved_seconds: int = Field(default=0, ge=0)
    consumed_seconds: int = Field(default=0, ge=0)


def make_run(
    number: int,
    *,
    owner: str = OWNER_A,
    request: uuid.UUID | None = None,
    **overrides: Any,
) -> RunRecord:
    """A queued run whose id and request key follow from number, so tests can place shards."""
    return run_record(
        run_id=f"run_{number:032x}",
        owner_id=owner,
        request_uuid=request or uuid.UUID(int=number),
        **overrides,
    )


def idem_for(run: RunRecord, *, request: uuid.UUID | None = None) -> IdempotencyRecord:
    return IdempotencyRecord(
        owner_id=run.owner_id,
        request_uuid=request or run.request_uuid,
        digest=run.manifest_digest,
        run_id=run.run_id,
        created_at=EPOCH,
        expires_at=EPOCH + timedelta(days=1),
    )


def firestore_pair(
    *,
    clock: ManualClock | None = None,
    strict: int | None = None,
    shards: int = ADMISSION_SHARDS,
) -> tuple[FirestoreRunStore, FakeFirestore]:
    fake = FakeFirestore(clock=clock.now if clock else None, strict_window_limit=strict)
    provision_admission_layout(fake, shards=shards)
    return FirestoreRunStore(fake, shards=shards), fake


def numbers_covering_shards(shards: int, *, start: int = 1) -> list[int]:
    """One run number for every shard, found by scanning upward from start."""
    chosen: dict[int, int] = {}
    number = start
    while len(chosen) < shards:
        chosen.setdefault(shard_index(f"run_{number:032x}", shards), number)
        number += 1
    return [chosen[index] for index in range(shards)]


def shard_ids(shards: int) -> list[str]:
    return [f"{index:03d}" for index in range(shards)]


@pytest.fixture(params=["memory", "firestore"])
def store(request: pytest.FixtureRequest) -> RunStore:
    if request.param == "memory":
        return InMemoryRunStore()
    return firestore_pair()[0]


# Parity with the in-memory store ----------------------------------------


def test_created_run_and_its_keys_read_back(store: RunStore) -> None:
    run = make_run(1)
    idem = idem_for(run)
    store.create_run(run, idem)
    assert store.get_run(run.run_id) == run
    assert store.find_run_by_request(run.owner_id, run.request_uuid) == run
    assert store.get_idempotency(run.owner_id, run.request_uuid) == idem


def test_unknown_keys_read_as_absent(store: RunStore) -> None:
    run = make_run(1)
    assert store.get_run(run.run_id) is None
    assert store.find_run_by_request(OWNER_A, run.request_uuid) is None
    assert store.get_idempotency(OWNER_A, run.request_uuid) is None
    assert store.restore_floor() is None
    assert store.list_runs() == []


def test_duplicate_run_id_is_refused_and_the_original_stands(store: RunStore) -> None:
    first = make_run(1)
    store.create_run(first, idem_for(first))
    same_id = make_run(1, owner=OWNER_B)
    with pytest.raises(ProtocolError) as exc:
        store.create_run(same_id, idem_for(same_id))
    assert exc.value.code is ErrorCode.VERSION_CONFLICT
    assert store.get_run(first.run_id) == first


def test_duplicate_request_key_is_refused(store: RunStore) -> None:
    first = make_run(1)
    store.create_run(first, idem_for(first))
    replay = make_run(2, request=first.request_uuid)
    with pytest.raises(ProtocolError) as exc:
        store.create_run(replay, idem_for(replay))
    assert exc.value.code is ErrorCode.VERSION_CONFLICT
    assert store.find_run_by_request(OWNER_A, first.request_uuid) == first


def test_same_request_uuid_under_another_owner_is_a_different_request(
    store: RunStore,
) -> None:
    a = make_run(1, owner=OWNER_A)
    b = make_run(2, owner=OWNER_B, request=a.request_uuid)
    store.create_run(a, idem_for(a))
    store.create_run(b, idem_for(b))
    assert store.find_run_by_request(OWNER_B, a.request_uuid) == b
    assert store.find_run_by_request(OWNER_A, a.request_uuid) == a


def test_stale_version_is_refused_and_the_stored_run_stands(store: RunStore) -> None:
    run = make_run(1)
    store.create_run(run, idem_for(run))
    dispatching = run.model_copy(update={"version": 2, "state": RunState.DISPATCHING})
    store.compare_and_set(dispatching, expected_version=1)
    stale = run.model_copy(update={"version": 2, "state": RunState.CANCELLED})
    with pytest.raises(ProtocolError) as exc:
        store.compare_and_set(stale, expected_version=1)
    assert exc.value.code is ErrorCode.VERSION_CONFLICT
    assert store.get_run(run.run_id) == dispatching


def test_execution_name_reads_back_and_a_stale_write_cannot_replace_it(
    store: RunStore,
) -> None:
    name = "projects/mao-dev/locations/us-central1/jobs/mao-worker/executions/mao-worker-7f2kq"
    run = make_run(1)
    store.create_run(run, idem_for(run))
    launched = run.model_copy(
        update={"version": 2, "state": RunState.DISPATCHING, "execution_name": name}
    )
    store.compare_and_set(launched, expected_version=1)
    assert store.get_run(run.run_id) == launched
    stale = run.model_copy(
        update={"version": 2, "execution_name": name.replace("7f2kq", "9x4pd")}
    )
    with pytest.raises(ProtocolError) as exc:
        store.compare_and_set(stale, expected_version=1)
    assert exc.value.code is ErrorCode.VERSION_CONFLICT
    stored = store.get_run(run.run_id)
    assert stored is not None
    assert stored.execution_name == name


@pytest.mark.parametrize("update", [False, True], ids=["create", "compare-and-set"])
def test_unset_execution_name_keeps_actual_stored_records_readable_by_the_previous_reader(
    update: bool,
) -> None:
    store, fake = firestore_pair()
    run = make_run(1)
    store.create_run(run, idem_for(run))
    if update:
        run = run.model_copy(update={"version": 2, "consumed_seconds": 5})
        store.compare_and_set(run, expected_version=1)
    document = fake.document("runs", run.run_id)
    assert document is not None
    record = document["record"]

    previous = _PreviousRunRecord.model_validate(record)

    assert previous.run_id == run.run_id
    assert previous.version == run.version
    assert "execution_name" not in record
    for name in ("last_seen_at", "last_observed_at", "failure", "next_reconcile_at"):
        assert name in record
        assert record[name] is None
    assert store.get_run(run.run_id) == run


@pytest.mark.parametrize("update", [False, True], ids=["create", "compare-and-set"])
def test_populated_execution_names_are_preserved_and_require_an_upgraded_reader(
    update: bool,
) -> None:
    name = "projects/mao-dev/locations/us-central1/jobs/mao-worker/executions/mao-worker-7f2kq"
    store, fake = firestore_pair()
    run = make_run(1, execution_name=None if update else name)
    store.create_run(run, idem_for(run))
    if update:
        run = run.model_copy(update={"version": 2, "execution_name": name})
        store.compare_and_set(run, expected_version=1)
    document = fake.document("runs", run.run_id)
    assert document is not None

    assert document["record"]["execution_name"] == name
    assert store.get_run(run.run_id) == run
    with pytest.raises(ValidationError) as exc:
        _PreviousRunRecord.model_validate(document["record"])
    assert any(
        error["type"] == "extra_forbidden" and error["loc"] == ("execution_name",)
        for error in exc.value.errors()
    )


def test_compare_and_set_on_an_unknown_run_is_not_found(store: RunStore) -> None:
    run = make_run(1)
    with pytest.raises(ProtocolError) as exc:
        store.compare_and_set(run, expected_version=1)
    assert exc.value.code is ErrorCode.NOT_FOUND


def test_revision_moves_on_create_and_on_compare_and_set(store: RunStore) -> None:
    run = make_run(1)
    before = store.admission_snapshot().revision
    store.create_run(run, idem_for(run))
    after_create = store.admission_snapshot().revision
    store.compare_and_set(run.model_copy(update={"version": 2}), expected_version=1)
    after_cas = store.admission_snapshot().revision
    assert before < after_create < after_cas


def test_run_created_after_a_snapshot_refuses_a_compare_and_set_on_that_snapshot(
    store: RunStore,
) -> None:
    run = make_run(1)
    store.create_run(run, idem_for(run))
    snapshot = store.admission_snapshot()
    intruder = make_run(2)
    store.create_run(intruder, idem_for(intruder))
    dispatching = run.model_copy(update={"version": 2, "state": RunState.DISPATCHING})
    with pytest.raises(ProtocolError) as exc:
        store.compare_and_set(
            dispatching, expected_version=1, expected_revision=snapshot.revision
        )
    assert exc.value.code is ErrorCode.VERSION_CONFLICT
    assert store.get_run(run.run_id) == run


def test_compare_and_set_with_the_current_revision_applies(store: RunStore) -> None:
    run = make_run(1)
    store.create_run(run, idem_for(run))
    snapshot = store.admission_snapshot()
    dispatching = run.model_copy(update={"version": 2, "state": RunState.DISPATCHING})
    store.compare_and_set(
        dispatching, expected_version=1, expected_revision=snapshot.revision
    )
    assert store.get_run(run.run_id) == dispatching


def test_retry_record_is_stored_under_its_own_key_and_claims_no_request(
    store: RunStore,
) -> None:
    run = make_run(1)
    create_idem = idem_for(run)
    store.create_run(run, create_idem)
    retry_uuid = uuid.UUID(int=99)
    retry_idem = idem_for(run, request=retry_uuid)
    updated = run.model_copy(update={"version": 2})
    store.compare_and_set(updated, expected_version=1, idempotency=retry_idem)
    assert store.get_idempotency(OWNER_A, retry_uuid) == retry_idem
    assert store.find_run_by_request(OWNER_A, retry_uuid) is None
    assert store.get_idempotency(OWNER_A, run.request_uuid) == create_idem
    assert store.find_run_by_request(OWNER_A, run.request_uuid) == updated


def test_list_runs_returns_every_created_run(store: RunStore) -> None:
    runs = [make_run(number) for number in range(1, 4)]
    for run in runs:
        store.create_run(run, idem_for(run))
    assert sorted(run.run_id for run in store.list_runs()) == sorted(
        run.run_id for run in runs
    )


def test_admission_choice_is_the_same_on_the_bounded_snapshot(store: RunStore) -> None:
    # Owner A's last dispatch is on an earlier day than the queue, so only the owner index brings
    # it into the Firestore snapshot. Owners with no dispatch go first, so B must win. Without A's
    # dispatch history A would win on queue age, because A's head entered the queue first.
    earlier_day = EPOCH - timedelta(days=3)
    done = make_run(1, owner=OWNER_A, admitted_at=earlier_day)
    store.create_run(done, idem_for(done))
    store.compare_and_set(
        done.model_copy(
            update={
                "version": 2,
                "state": RunState.SUCCEEDED,
                "dispatch_order": 1,
                "dispatched_at": earlier_day,
            }
        ),
        expected_version=1,
    )
    owner_a_next = make_run(
        2, owner=OWNER_A, state_entered_at=EPOCH + timedelta(seconds=1)
    )
    owner_b_head = make_run(
        3, owner=OWNER_B, state_entered_at=EPOCH + timedelta(seconds=2)
    )
    store.create_run(owner_a_next, idem_for(owner_a_next))
    store.create_run(owner_b_head, idem_for(owner_b_head))
    chosen = select_next_dispatch(
        store.admission_snapshot().records,
        now=EPOCH + timedelta(seconds=5),
        limits=admission_limits(),
    )
    assert chosen is not None
    assert chosen.run_id == owner_b_head.run_id


# Firestore layout ---------------------------------------------------------


def test_adapter_satisfies_the_run_store_port() -> None:
    store: RunStore = FirestoreRunStore(FakeFirestore())
    assert store is not None


def test_snapshot_reads_capacity_and_day_records_not_history() -> None:
    store, _ = firestore_pair()
    old_day = EPOCH - timedelta(days=3)
    for number in range(100, 140):
        old = make_run(number, admitted_at=old_day)
        store.create_run(old, idem_for(old))
        store.compare_and_set(
            old.model_copy(update={"version": 2, "state": RunState.SUCCEEDED}),
            expected_version=1,
        )
    queued = make_run(1)
    store.create_run(queued, idem_for(queued))
    same_day_done = make_run(2)
    store.create_run(same_day_done, idem_for(same_day_done))
    store.compare_and_set(
        same_day_done.model_copy(update={"version": 2, "state": RunState.SUCCEEDED}),
        expected_version=1,
    )
    ids = {run.run_id for run in store.admission_snapshot().records}
    assert ids == {queued.run_id, same_day_done.run_id}


def test_owner_index_keeps_the_latest_dispatch_and_never_moves_back() -> None:
    store, _ = firestore_pair()
    old_day = EPOCH - timedelta(days=3)
    first = make_run(1, admitted_at=old_day)
    second = make_run(2, admitted_at=old_day)
    for run in (first, second):
        store.create_run(run, idem_for(run))
    dispatched_first = first.model_copy(
        update={
            "version": 2,
            "state": RunState.SUCCEEDED,
            "dispatch_order": 1,
            "dispatched_at": old_day + timedelta(minutes=1),
        }
    )
    store.compare_and_set(dispatched_first, expected_version=1)
    dispatched_second = second.model_copy(
        update={
            "version": 2,
            "state": RunState.SUCCEEDED,
            "dispatch_order": 2,
            "dispatched_at": old_day + timedelta(minutes=2),
        }
    )
    store.compare_and_set(dispatched_second, expected_version=1)
    assert {run.run_id for run in store.admission_snapshot().records} == {second.run_id}
    store.compare_and_set(
        dispatched_first.model_copy(update={"version": 3, "consumed_seconds": 5}),
        expected_version=2,
    )
    assert {run.run_id for run in store.admission_snapshot().records} == {second.run_id}


def test_unprovisioned_layout_refuses_every_operation() -> None:
    fake = FakeFirestore()
    store = FirestoreRunStore(fake)
    run = make_run(1)
    with pytest.raises(FirestoreStoreError):
        store.create_run(run, idem_for(run))
    with pytest.raises(FirestoreStoreError):
        store.admission_snapshot()
    assert fake.documents("runs") == []


def test_a_different_shard_count_refuses_every_operation() -> None:
    fake = FakeFirestore()
    provision_admission_layout(fake, shards=8)
    with pytest.raises(FirestoreStoreError):
        FirestoreRunStore(fake, shards=16).admission_snapshot()


def test_provisioning_is_idempotent_for_one_count_and_refuses_another() -> None:
    fake = FakeFirestore()
    provision_admission_layout(fake, shards=ADMISSION_SHARDS)
    provision_admission_layout(fake, shards=ADMISSION_SHARDS)
    with pytest.raises(FirestoreStoreError):
        provision_admission_layout(fake, shards=8)


@pytest.mark.parametrize("shards", [0, -1, -16])
def test_invalid_shard_count_is_refused_before_opening_a_transaction(
    shards: int, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeFirestore()

    def unexpected_transaction(work: Any) -> Any:
        pytest.fail("invalid provisioning must not open a transaction")

    monkeypatch.setattr(fake, "run_transaction", unexpected_transaction)

    with pytest.raises(ValueError, match="shards must be at least 1"):
        provision_admission_layout(fake, shards=shards)

    assert fake.document(firestore_store.LAYOUT, "current") is None


@pytest.mark.parametrize("provisioned_shards", [None, 8])
@pytest.mark.parametrize(
    "operation",
    [
        "get_run",
        "find_run_by_request",
        "get_idempotency",
        "restore_floor",
        "admission_snapshot",
        "list_runs",
        "create_run",
        "compare_and_set",
    ],
)
def test_every_store_operation_refuses_an_absent_or_mismatched_layout(
    provisioned_shards: int | None, operation: str
) -> None:
    fake = FakeFirestore()
    if provisioned_shards is not None:
        provision_admission_layout(fake, shards=provisioned_shards)
    store = FirestoreRunStore(fake, shards=16)
    run = make_run(1)
    operations: dict[str, Callable[[], object]] = {
        "get_run": lambda: store.get_run(run.run_id),
        "find_run_by_request": lambda: store.find_run_by_request(
            OWNER_A, run.request_uuid
        ),
        "get_idempotency": lambda: store.get_idempotency(OWNER_A, run.request_uuid),
        "restore_floor": store.restore_floor,
        "admission_snapshot": store.admission_snapshot,
        "list_runs": store.list_runs,
        "create_run": lambda: store.create_run(run, idem_for(run)),
        "compare_and_set": lambda: store.compare_and_set(run, expected_version=1),
    }

    with pytest.raises(FirestoreStoreError):
        operations[operation]()

    assert fake.documents(firestore_store.RUNS) == []


def test_dispatched_create_and_update_read_before_writing() -> None:
    store, _ = firestore_pair()
    run = make_run(1, dispatch_order=1, dispatched_at=EPOCH)
    store.create_run(run, idem_for(run))
    updated = run.model_copy(update={"version": 2, "dispatch_order": 2})

    store.compare_and_set(updated, expected_version=1)

    assert store.get_run(run.run_id) == updated
    assert store.admission_snapshot().revision == 2


def test_every_mutation_advances_exactly_one_shard() -> None:
    store, fake = firestore_pair()
    run = make_run(1)
    store.create_run(run, idem_for(run))
    store.compare_and_set(run.model_copy(update={"version": 2}), expected_version=1)
    shard_writes = sum(
        fake.total_writes(SHARDS, shard) for shard in shard_ids(ADMISSION_SHARDS)
    )
    assert shard_writes == 2
    assert store.admission_snapshot().revision == 2


def test_admission_compare_and_set_retries_then_refuses_after_a_concurrent_mutation() -> (
    None
):
    store, fake = firestore_pair()
    run = make_run(1)
    store.create_run(run, idem_for(run))
    snapshot = store.admission_snapshot()
    intruder = make_run(2)
    fake.interleave(lambda: store.create_run(intruder, idem_for(intruder)))
    aborts_before = fake.aborts
    with pytest.raises(ProtocolError) as exc:
        store.compare_and_set(
            run.model_copy(update={"version": 2, "state": RunState.DISPATCHING}),
            expected_version=1,
            expected_revision=snapshot.revision,
        )
    assert exc.value.code is ErrorCode.VERSION_CONFLICT
    assert fake.aborts - aborts_before == 1
    assert store.get_run(run.run_id) == run


def test_plain_mutation_does_not_contend_with_a_mutation_on_another_shard() -> None:
    store, fake = firestore_pair()
    first_number, second_number = numbers_covering_shards(ADMISSION_SHARDS)[:2]
    a, b = make_run(first_number), make_run(second_number)
    store.create_run(a, idem_for(a))
    fake.interleave(lambda: store.create_run(b, idem_for(b)))
    aborts_before = fake.aborts
    store.compare_and_set(a.model_copy(update={"version": 2}), expected_version=1)
    assert fake.aborts == aborts_before
    assert store.get_run(b.run_id) == b


def test_restore_floor_reads_the_stored_floor() -> None:
    store, fake = firestore_pair()
    fake.run_transaction(
        lambda txn: txn.set("meta", "restore_floor", {"floor": EPOCH.isoformat()})
    )
    assert store.restore_floor() == EPOCH


# Per-document write rates -------------------------------------------------


def test_single_revision_document_refuses_a_second_mutation_in_one_second() -> None:
    clock = ManualClock()
    store, _ = firestore_pair(clock=clock, strict=1, shards=1)
    first, second = make_run(1), make_run(2)
    store.create_run(first, idem_for(first))
    with pytest.raises(ProtocolError) as exc:
        store.create_run(second, idem_for(second))
    assert exc.value.code is ErrorCode.VERSION_CONFLICT


def test_single_revision_document_takes_every_mutation_in_one_second() -> None:
    clock = ManualClock()
    store, fake = firestore_pair(clock=clock, shards=1)
    for number in range(1, 17):
        run = make_run(number)
        store.create_run(run, idem_for(run))
    assert fake.peak_writes_per_second(SHARDS, "000") == 16


def test_sixteen_shards_take_one_mutation_each_in_the_same_second() -> None:
    clock = ManualClock()
    store, fake = firestore_pair(clock=clock, strict=1)
    for number in numbers_covering_shards(ADMISSION_SHARDS):
        run = make_run(number)
        store.create_run(run, idem_for(run))
    peaks = [
        fake.peak_writes_per_second(SHARDS, shard)
        for shard in shard_ids(ADMISSION_SHARDS)
    ]
    assert peaks == [1] * ADMISSION_SHARDS
    assert store.admission_snapshot().revision == ADMISSION_SHARDS


def test_a_seventeenth_mutation_in_one_second_must_share_a_shard() -> None:
    clock = ManualClock()
    store, _ = firestore_pair(clock=clock, strict=1)
    for number in numbers_covering_shards(ADMISSION_SHARDS):
        run = make_run(number)
        store.create_run(run, idem_for(run))
    extra = make_run(10_000)
    with pytest.raises(ProtocolError) as exc:
        store.create_run(extra, idem_for(extra))
    assert exc.value.code is ErrorCode.VERSION_CONFLICT


def test_sustained_one_mutation_per_shard_per_second_stays_at_the_limit() -> None:
    clock = ManualClock()
    store, fake = firestore_pair(clock=clock, strict=1)
    seconds = 10
    for second in range(seconds):
        clock.advance(seconds=1)
        for number in numbers_covering_shards(
            ADMISSION_SHARDS, start=1 + second * 1000
        ):
            run = make_run(number)
            store.create_run(run, idem_for(run))
    for shard in shard_ids(ADMISSION_SHARDS):
        assert fake.peak_writes_per_second(SHARDS, shard) == 1
        assert fake.total_writes(SHARDS, shard) == seconds
    assert store.admission_snapshot().revision == seconds * ADMISSION_SHARDS


# Fake and module guards ---------------------------------------------------


@pytest.mark.parametrize("write", ["create", "set"])
@pytest.mark.parametrize("read", ["get", "query", "scan"])
def test_fake_refuses_every_read_after_a_staged_write(write: str, read: str) -> None:
    fake = FakeFirestore()

    def work(txn: Any) -> None:
        getattr(txn, write)("runs", "written", {"state": "queued"})
        if read == "get":
            txn.get("other", "missing")
        elif read == "query":
            txn.query("other", "state", "queued")
        else:
            txn.scan("other")

    with pytest.raises(AssertionError, match="read after write in one transaction"):
        fake.run_transaction(work)

    assert fake.document("runs", "written") is None


def test_fake_queries_do_not_detect_phantom_inserts() -> None:
    fake = FakeFirestore()
    fake.run_transaction(lambda txn: txn.create("runs", "seed", {"state": "queued"}))

    def insert_matching_document() -> None:
        fake.run_transaction(
            lambda txn: txn.create("runs", "phantom", {"state": "queued"})
        )

    fake.interleave(insert_matching_document)
    result = fake.run_transaction(lambda txn: txn.query("runs", "state", "queued"))
    assert [doc_id for doc_id, _ in result] == ["seed"]
    assert fake.aborts == 0
    assert fake.document("runs", "phantom") is not None


@pytest.mark.parametrize("value", [{"at": EPOCH}, {"state": RunState.QUEUED}])
def test_fake_refuses_values_that_are_not_plain_json(value: dict[str, Any]) -> None:
    fake = FakeFirestore()
    with pytest.raises(TypeError):
        fake.run_transaction(lambda txn: txn.set("runs", "x", value))


def test_adapter_module_does_not_import_the_firestore_package_at_load() -> None:
    tree = ast.parse(Path(firestore_store.__file__).read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.Import):
            assert not any(alias.name.startswith("google") for alias in node.names)
        if isinstance(node, ast.ImportFrom):
            assert not (node.module or "").startswith("google")
