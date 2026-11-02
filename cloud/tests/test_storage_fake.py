"""Contract tests for the in-memory Storage fake (DPES-869 AC1 at fake level, conformance stays a gap)."""

import pytest

from fakes import RUN_ID, run_record
from mao_cloud.protocol import ErrorCode, ProtocolError
from mao_cloud.storage import (
    DurableBatch,
    DurableKind,
    DurableWrite,
    StorageConflict,
    StorageFailure,
)
from storage_fakes import InMemoryStorage

MANIFEST_NAME = f"runs/{RUN_ID}/attempts/000001/checkpoints/ck-0001/manifest.json"
KEYS = (
    (DurableKind.TRANSCRIPT_ENTRY, "transcript:a:1"),
    (DurableKind.DOCUMENT, "document:a"),
    (DurableKind.TASK, "task:a"),
)


def _batch(tag: str) -> DurableBatch:
    return DurableBatch(
        run_id=RUN_ID,
        writes=(
            DurableWrite(
                DurableKind.TRANSCRIPT_ENTRY,
                f"transcript:{tag}:1",
                f"entry-{tag}".encode(),
            ),
            DurableWrite(
                DurableKind.DOCUMENT, f"document:{tag}", f"doc-{tag}".encode()
            ),
            DurableWrite(DurableKind.TASK, f"task:{tag}", f"task-{tag}".encode()),
        ),
    )


def test_a_committed_batch_reads_back_for_every_kind_after_reopen() -> None:
    storage = InMemoryStorage()
    storage.commit(_batch("a"))
    reopened = storage.reopen()
    assert (
        reopened.read_durable(RUN_ID, DurableKind.TRANSCRIPT_ENTRY, "transcript:a:1")
        == b"entry-a"
    )
    assert reopened.read_durable(RUN_ID, DurableKind.DOCUMENT, "document:a") == b"doc-a"
    assert reopened.read_durable(RUN_ID, DurableKind.TASK, "task:a") == b"task-a"


@pytest.mark.parametrize("failing_index", [0, 1, 2])
def test_a_batch_failing_at_any_write_commits_nothing(failing_index: int) -> None:
    storage = InMemoryStorage()
    storage.fail_commit_at(failing_index)
    with pytest.raises(StorageFailure):
        storage.commit(_batch("a"))
    reopened = storage.reopen()
    for kind, key in KEYS:
        assert reopened.read_durable(RUN_ID, kind, key) is None


def test_a_failed_batch_leaves_the_prior_batch_intact() -> None:
    storage = InMemoryStorage()
    storage.commit(_batch("a"))
    storage.fail_commit_at(1)
    with pytest.raises(StorageFailure):
        storage.commit(_batch("b"))
    reopened = storage.reopen()
    assert reopened.read_durable(RUN_ID, DurableKind.DOCUMENT, "document:a") == b"doc-a"
    assert reopened.read_durable(RUN_ID, DurableKind.TASK, "task:b") is None
    assert reopened.read_durable(RUN_ID, DurableKind.DOCUMENT, "document:b") is None


def test_a_batch_whose_key_is_already_stored_is_refused_whole() -> None:
    storage = InMemoryStorage()
    storage.commit(_batch("a"))
    clash = DurableBatch(
        run_id=RUN_ID,
        writes=(
            DurableWrite(DurableKind.TASK, "task:new", b"new"),
            DurableWrite(DurableKind.DOCUMENT, "document:a", b"other"),
        ),
    )
    with pytest.raises(StorageConflict):
        storage.commit(clash)
    reopened = storage.reopen()
    assert reopened.read_durable(RUN_ID, DurableKind.TASK, "task:new") is None
    assert reopened.read_durable(RUN_ID, DurableKind.DOCUMENT, "document:a") == b"doc-a"


def test_a_batch_repeating_a_key_inside_itself_is_refused_whole() -> None:
    storage = InMemoryStorage()
    repeated = DurableBatch(
        run_id=RUN_ID,
        writes=(
            DurableWrite(DurableKind.TASK, "task:1", b"one"),
            DurableWrite(DurableKind.TASK, "task:1", b"two"),
        ),
    )
    with pytest.raises(StorageConflict):
        storage.commit(repeated)
    assert storage.reopen().read_durable(RUN_ID, DurableKind.TASK, "task:1") is None


def test_a_create_only_object_cannot_be_overwritten() -> None:
    storage = InMemoryStorage()
    generation = storage.create_object("runs/x/result.json", b"first")
    with pytest.raises(StorageConflict):
        storage.create_object("runs/x/result.json", b"second")
    assert storage.read_object("runs/x/result.json") == b"first"
    head = storage.head("runs/x/result.json")
    assert head is not None
    assert head.generation == generation


def test_a_transaction_that_raises_applies_nothing() -> None:
    storage = InMemoryStorage()
    run = run_record()
    storage.seed_run(run)
    with pytest.raises(RuntimeError):
        with storage.transaction() as txn:
            txn.put_run(
                run.model_copy(update={"version": run.version + 1}),
                expected_version=run.version,
            )
            txn.put_pointer(RUN_ID, MANIFEST_NAME)
            raise RuntimeError("fault after staging")
    reopened = storage.reopen()
    assert reopened.read_run(RUN_ID) == run
    assert reopened.read_pointer(RUN_ID) is None


def test_a_transaction_that_completes_applies_every_staged_write() -> None:
    storage = InMemoryStorage()
    run = run_record()
    storage.seed_run(run)
    updated = run.model_copy(update={"version": run.version + 1})
    with storage.transaction() as txn:
        txn.put_run(updated, expected_version=run.version)
        txn.put_pointer(RUN_ID, MANIFEST_NAME)
    reopened = storage.reopen()
    assert reopened.read_run(RUN_ID) == updated
    assert reopened.read_pointer(RUN_ID) == MANIFEST_NAME


def test_a_transaction_put_run_against_a_moved_version_is_refused_and_applies_nothing() -> (
    None
):
    storage = InMemoryStorage()
    run = run_record()
    storage.seed_run(run)
    with pytest.raises(ProtocolError) as info:
        with storage.transaction() as txn:
            txn.put_run(run.model_copy(update={"version": 3}), expected_version=2)
            txn.put_pointer(RUN_ID, MANIFEST_NAME)
    assert info.value.code is ErrorCode.VERSION_CONFLICT
    reopened = storage.reopen()
    assert reopened.read_run(RUN_ID) == run
    assert reopened.read_pointer(RUN_ID) is None


def test_checkpoint_component_bytes_counts_only_one_runs_harness_and_workspace_objects() -> (
    None
):
    storage = InMemoryStorage()
    storage.seed_run(run_record())
    prefix = f"runs/{RUN_ID}/attempts/000001/checkpoints/ck-0001"
    storage.create_object(f"{prefix}/harness.bin", b"12345")
    storage.create_object(f"{prefix}/workspace.tar", b"123")
    storage.create_object(f"{prefix}/manifest.json", b"{}")
    storage.create_object(
        "runs/run_other/attempts/000001/checkpoints/ck-0001/harness.bin",
        b"1234567",
    )
    assert storage.checkpoint_component_bytes(RUN_ID) == 8
    assert storage.checkpoint_component_bytes("run_unknown") == 0


def test_checkpoint_reservations_are_shared_replayable_and_all_or_none() -> None:
    storage = InMemoryStorage()
    prefix = f"runs/{RUN_ID}/attempts/000001/checkpoints"
    first = {f"{prefix}/ck-0001/harness.bin": 10, f"{prefix}/ck-0001/workspace.tar.gz": 10}
    second = {f"{prefix}/ck-0002/harness.bin": 10, f"{prefix}/ck-0002/workspace.tar.gz": 10}
    assert storage.reserve_checkpoint_components(RUN_ID, first, max_bytes=30)
    reopened = storage.reopen()
    assert reopened.reserve_checkpoint_components(RUN_ID, first, max_bytes=30)
    assert not reopened.reserve_checkpoint_components(RUN_ID, second, max_bytes=30)
    assert reopened.reserve_checkpoint_components(
        RUN_ID, {f"{prefix}/ck-0003/harness.bin": 10}, max_bytes=30
    )
    assert not storage.reserve_checkpoint_components(
        RUN_ID, {f"{prefix}/ck-0004/harness.bin": 1}, max_bytes=30
    )


def test_uploaded_reserved_components_are_not_counted_twice() -> None:
    storage = InMemoryStorage()
    name = f"runs/{RUN_ID}/attempts/000001/checkpoints/ck-0001/harness.bin"
    assert storage.reserve_checkpoint_components(RUN_ID, {name: 10}, max_bytes=10)
    storage.create_object(name, b"h" * 10)
    assert storage.reopen().reserve_checkpoint_components(RUN_ID, {name: 10}, max_bytes=10)
