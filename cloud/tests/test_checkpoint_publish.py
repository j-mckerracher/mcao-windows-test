"""Checkpoint and result publication over the Storage fake (DPES-869 AC3 and AC6, fake level)."""

import dataclasses
import json
from collections.abc import Callable
from contextlib import contextmanager
from datetime import timedelta
from typing import Any

import pytest

from fakes import EPOCH, IMAGE_DIGEST, RUN_ID, run_record
from mao_cloud import checkpoint
from mao_cloud.checkpoint import (
    CheckpointError,
    CheckpointErrorCode,
    CheckpointManifest,
    CommitMarker,
    ObjectRef,
    PublicationBoundary,
    WorkspaceEntry,
)
from mao_cloud.checkpoint_publish import (
    CheckpointDraft,
    publish_checkpoint,
    publish_result,
    read_selection,
)
from mao_cloud.protocol import (
    MAX_CHECKPOINT_BYTES,
    MAX_RESULT_BYTES,
    ErrorCode,
    ObjectKind,
    ProtocolError,
    RunRecord,
    RunResultDocument,
    RunState,
    checkpoint_object_names,
    object_name,
    sha256_hex,
)
from mao_cloud.storage import StorageFailure
from storage_fakes import FaultingStorage, InMemoryStorage

CLAIM_ID = "claim_" + "e" * 32
BASE_SHA = "1" * 40
SOURCE_DIGEST = "a" * 64
HARNESS_VERSION = "1.3.0"
NOW = EPOCH + timedelta(seconds=1)
SENTINEL = "SIGNED-URL-SENTINEL-c41e"
HARNESS = b"harness-state-v1"
WORKSPACE = b"workspace-archive-v1"
RESULT_NAME = object_name(ObjectKind.RESULT, run_id=RUN_ID, attempt=1)

# Every PublicationBoundary and the fault that realizes it. The assertion below keeps this exhaustive.
FAULTS: dict[PublicationBoundary, dict[str, Any]] = {
    PublicationBoundary.HARNESS_STATE_WRITTEN: {"after_create": "/harness.bin"},
    PublicationBoundary.WORKSPACE_STATE_WRITTEN: {"after_create": "/workspace.tar.gz"},
    PublicationBoundary.MANIFEST_WRITTEN: {"after_create": "/manifest.json"},
    PublicationBoundary.POINTER_TRANSACTION_FAILED: {"after_transaction_body": True},
}


def test_every_publication_boundary_has_a_fault() -> None:
    assert set(FAULTS) == set(PublicationBoundary)


def _names(checkpoint_id: str) -> dict[str, str]:
    base = f"runs/{RUN_ID}/attempts/000001/checkpoints/{checkpoint_id}"
    return {
        "harness": f"{base}/harness.bin",
        "workspace": f"{base}/workspace.tar.gz",
        "manifest": f"{base}/manifest.json",
    }


def _current(**overrides: Any) -> RunRecord:
    values: dict[str, Any] = {
        "state": RunState.RUNNING,
        "claim_id": CLAIM_ID,
        "attempt": 1,
        "fence": 1,
        "version": 4,
        "last_seen_at": EPOCH,
    }
    values.update(overrides)
    return run_record(**values)


def _storage(run: RunRecord | None = None) -> InMemoryStorage:
    storage = InMemoryStorage()
    storage.seed_run(run if run is not None else _current())
    return storage


def _stored(storage: InMemoryStorage) -> RunRecord:
    run = storage.read_run(RUN_ID)
    assert run is not None
    return run


def _draft(
    checkpoint_id: str = "ck-0001", *, harness: bytes = HARNESS
) -> CheckpointDraft:
    return CheckpointDraft(
        run_id=RUN_ID,
        attempt=1,
        fence=1,
        claim_id=CLAIM_ID,
        checkpoint_id=checkpoint_id,
        base_sha=BASE_SHA,
        source_digest=SOURCE_DIGEST,
        harness="pi",
        harness_version=HARNESS_VERSION,
        image_digest=IMAGE_DIGEST,
        harness_bytes=harness,
        workspace_bytes=WORKSPACE,
        workspace_entries=(
            WorkspaceEntry(path="src/app.py", kind="file", executable=False),
        ),
    )


def _select(storage: InMemoryStorage) -> Any:
    return read_selection(
        storage,
        _stored(storage),
        harness_version=HARNESS_VERSION,
        image_digest=IMAGE_DIGEST,
        expected_attempt=1,
        expected_version=4,
    )


def _result(*, attempt: int = 1) -> RunResultDocument:
    return RunResultDocument(
        protocol_version="v1",
        run_id=RUN_ID,
        attempt=attempt,
        fence=1,
        claim_id=CLAIM_ID,
        outcome="succeeded",
        exit_code=0,
        commit_sha=BASE_SHA,
    )


def _ref(name: str, data: bytes, generation: int) -> ObjectRef:
    return ObjectRef(
        object_name=name,
        generation=generation,
        sha256=sha256_hex(data),
        size_bytes=len(data),
    )


def test_a_published_checkpoint_is_selected_after_reopen() -> None:
    storage = _storage()
    marker = publish_checkpoint(
        storage, _draft(), expected_attempt=1, expected_version=4
    )
    assert _select(storage.reopen()).marker == marker


def test_publishing_leaves_the_run_version_unchanged() -> None:
    storage = _storage()
    publish_checkpoint(storage, _draft(), expected_attempt=1, expected_version=4)
    assert _stored(storage).version == 4


def test_components_are_created_before_the_manifest_and_the_commit_comes_last() -> None:
    names = _names("ck-0001")
    storage = FaultingStorage(_storage())
    publish_checkpoint(storage, _draft(), expected_attempt=1, expected_version=4)
    assert storage.events == [
        f"create:{names['harness']}",
        f"create:{names['workspace']}",
        f"create:{names['manifest']}",
        "transaction",
    ]


@pytest.mark.parametrize("boundary", list(PublicationBoundary))
def test_a_fault_at_any_publication_boundary_keeps_the_prior_selection(
    boundary: PublicationBoundary,
) -> None:
    inner = _storage()
    prior = publish_checkpoint(
        inner, _draft("ck-0001"), expected_attempt=1, expected_version=4
    )
    with pytest.raises(CheckpointError) as info:
        publish_checkpoint(
            FaultingStorage(inner, **FAULTS[boundary]),
            _draft("ck-0002"),
            expected_attempt=1,
            expected_version=4,
        )
    assert info.value.code is CheckpointErrorCode.COMMIT_FAILED
    reopened = inner.reopen()
    assert _select(reopened).marker == prior
    assert reopened.read_marker(_names("ck-0002")["manifest"]) is None


@pytest.mark.parametrize("boundary", list(PublicationBoundary))
def test_a_fault_on_the_first_checkpoint_selects_nothing(
    boundary: PublicationBoundary,
) -> None:
    inner = _storage()
    with pytest.raises(CheckpointError) as info:
        publish_checkpoint(
            FaultingStorage(inner, **FAULTS[boundary]),
            _draft(),
            expected_attempt=1,
            expected_version=4,
        )
    assert info.value.code is CheckpointErrorCode.COMMIT_FAILED
    with pytest.raises(CheckpointError) as selected:
        _select(inner.reopen())
    assert selected.value.code is CheckpointErrorCode.MISSING


@pytest.mark.parametrize("boundary", list(PublicationBoundary))
def test_a_partial_publish_is_adopted_by_a_same_id_retry_and_selects_nothing_before_it(
    boundary: PublicationBoundary,
) -> None:
    """Protocol-v1 section 15.5 decision 1: an identical component left by a failed try is adopted.

    The failed id's leftovers select nothing until a retry with the same draft commits. The retry
    then selects exactly its own marker.
    """
    inner = _storage()
    with pytest.raises(CheckpointError) as first:
        publish_checkpoint(
            FaultingStorage(inner, **FAULTS[boundary]),
            _draft("ck-0001"),
            expected_attempt=1,
            expected_version=4,
        )
    assert first.value.code is CheckpointErrorCode.COMMIT_FAILED
    assert inner.read_pointer(RUN_ID) is None

    marker = publish_checkpoint(
        inner, _draft("ck-0001"), expected_attempt=1, expected_version=4
    )
    reopened = inner.reopen()
    assert _select(reopened).marker == marker
    assert reopened.read_pointer(RUN_ID) == _names("ck-0001")["manifest"]


@pytest.mark.parametrize(
    "unreadable", ["/harness.bin", "/workspace.tar.gz", "/manifest.json"]
)
def test_a_component_that_does_not_read_back_is_never_committed(
    unreadable: str,
) -> None:
    inner = _storage()
    with pytest.raises(CheckpointError) as info:
        publish_checkpoint(
            FaultingStorage(inner, unreadable=unreadable),
            _draft(),
            expected_attempt=1,
            expected_version=4,
        )
    assert info.value.code is CheckpointErrorCode.MISSING
    assert inner.read_pointer(RUN_ID) is None
    assert inner.read_marker(_names("ck-0001")["manifest"]) is None


def test_a_publish_against_a_moved_version_is_refused_before_any_upload() -> None:
    storage = FaultingStorage(_storage())
    with pytest.raises(ProtocolError) as info:
        publish_checkpoint(storage, _draft(), expected_attempt=1, expected_version=3)
    assert info.value.code is ErrorCode.VERSION_CONFLICT
    assert storage.events == []


def test_a_stale_attempt_is_refused_before_any_upload() -> None:
    inner = _storage(_current(attempt=2))
    storage = FaultingStorage(inner)
    with pytest.raises(ProtocolError) as info:
        publish_checkpoint(storage, _draft(), expected_attempt=1, expected_version=4)
    assert info.value.code is ErrorCode.STALE_ATTEMPT
    assert storage.events == []
    assert inner.object_names() == []


def test_a_fence_moved_during_upload_is_refused_in_the_commit_transaction() -> None:
    inner = _storage()

    def move_fence_after_manifest(name: str) -> None:
        if name.endswith("/manifest.json"):
            inner.seed_run(_current(fence=2))

    storage = FaultingStorage(inner, on_create=move_fence_after_manifest)
    with pytest.raises(ProtocolError) as info:
        publish_checkpoint(storage, _draft(), expected_attempt=1, expected_version=4)
    assert info.value.code is ErrorCode.STALE_ATTEMPT
    assert inner.read_pointer(RUN_ID) is None
    assert inner.read_marker(_names("ck-0001")["manifest"]) is None


def test_a_stale_refusal_names_no_payload_or_credential() -> None:
    inner = _storage(_current(attempt=2))
    with pytest.raises(ProtocolError) as info:
        publish_checkpoint(
            inner,
            _draft(harness=SENTINEL.encode()),
            expected_attempt=1,
            expected_version=4,
        )
    assert SENTINEL not in str(info.value)
    assert SENTINEL not in json.dumps(info.value.detail, default=str)
    assert SENTINEL not in json.dumps(inner.lifecycle_metadata())


def test_the_sentinel_is_stored_in_the_payload_and_never_in_lifecycle_metadata() -> (
    None
):
    storage = _storage()
    publish_checkpoint(
        storage,
        _draft(harness=SENTINEL.encode()),
        expected_attempt=1,
        expected_version=4,
    )
    assert storage.read_object(_names("ck-0001")["harness"]) == SENTINEL.encode()
    metadata = json.dumps(storage.lifecycle_metadata())
    assert SENTINEL not in metadata
    assert "https://" not in metadata


def test_a_hand_committed_mixed_pair_is_refused_on_read() -> None:
    storage = _storage()
    harness_name = _names("ck-0001")["harness"]
    workspace_name = _names("ck-0002")["workspace"]
    manifest_name = _names("ck-0003")["manifest"]
    harness_generation = storage.create_object(harness_name, HARNESS)
    workspace_generation = storage.create_object(workspace_name, WORKSPACE)
    manifest = CheckpointManifest(
        schema_version=1,
        run_id=RUN_ID,
        attempt=1,
        fence=1,
        claim_id=CLAIM_ID,
        base_sha=BASE_SHA,
        source_digest=SOURCE_DIGEST,
        harness="pi",
        harness_version=HARNESS_VERSION,
        image_digest=IMAGE_DIGEST,
        harness_state=_ref(harness_name, HARNESS, harness_generation),
        workspace_state=_ref(workspace_name, WORKSPACE, workspace_generation),
    )
    data = manifest.model_dump_json().encode()
    manifest_generation = storage.create_object(manifest_name, data)
    marker = CommitMarker(
        run_id=RUN_ID,
        attempt=1,
        fence=1,
        claim_id=CLAIM_ID,
        manifest=_ref(manifest_name, data, manifest_generation),
        committed=True,
    )
    with storage.transaction() as txn:
        txn.put_marker(marker)
        txn.put_pointer(RUN_ID, manifest_name)
    with pytest.raises(CheckpointError) as info:
        _select(storage)
    assert info.value.code is CheckpointErrorCode.INTEGRITY_FAILED


def test_a_current_result_is_recorded_once_and_survives_reopen() -> None:
    storage = _storage()
    updated = publish_result(
        storage, _result(), body=b'{"outcome":"succeeded"}', now=NOW
    )
    assert updated.state is RunState.SUCCEEDED
    reopened = storage.reopen()
    assert _stored(reopened).state is RunState.SUCCEEDED
    assert reopened.read_object(RESULT_NAME) == b'{"outcome":"succeeded"}'


def test_a_repeat_result_is_refused_and_writes_no_second_object() -> None:
    storage = _storage()
    publish_result(storage, _result(), body=b"first", now=NOW)
    with pytest.raises(ProtocolError) as info:
        publish_result(storage, _result(), body=b"second", now=NOW)
    assert info.value.code is ErrorCode.TERMINAL_STATE
    assert storage.object_names() == [RESULT_NAME]


def test_a_stale_result_is_refused_before_upload_and_its_body_is_never_echoed() -> None:
    inner = _storage()
    storage = FaultingStorage(inner)
    with pytest.raises(ProtocolError) as info:
        publish_result(storage, _result(attempt=2), body=SENTINEL.encode(), now=NOW)
    assert info.value.code is ErrorCode.STALE_ATTEMPT
    assert storage.events == []
    assert inner.object_names() == []
    assert SENTINEL not in str(info.value)
    assert SENTINEL not in json.dumps(info.value.detail, default=str)
    assert SENTINEL not in json.dumps(inner.lifecycle_metadata())


# --- Retry after a partial upload (DPES-897 decision 1) -----------------------------------------


@pytest.mark.parametrize(
    "partial", ["/harness.bin", "/workspace.tar.gz", "/manifest.json"]
)
def test_a_retry_with_the_same_draft_adopts_the_components_and_commits(
    partial: str,
) -> None:
    inner = _storage()
    with pytest.raises(CheckpointError):
        publish_checkpoint(
            FaultingStorage(inner, after_create=partial),
            _draft(),
            expected_attempt=1,
            expected_version=4,
        )
    marker = publish_checkpoint(inner, _draft(), expected_attempt=1, expected_version=4)
    assert _select(inner.reopen()).marker == marker


def test_a_retry_whose_harness_bytes_differ_is_refused_and_commits_nothing() -> None:
    inner = _storage()
    with pytest.raises(CheckpointError):
        publish_checkpoint(
            FaultingStorage(inner, after_create="/harness.bin"),
            _draft(harness=HARNESS),
            expected_attempt=1,
            expected_version=4,
        )
    with pytest.raises(CheckpointError) as info:
        publish_checkpoint(
            inner,
            _draft(harness=b"harness-state-v2"),
            expected_attempt=1,
            expected_version=4,
        )
    assert info.value.code is CheckpointErrorCode.INTEGRITY_FAILED
    assert inner.read_pointer(RUN_ID) is None
    assert inner.read_marker(_names("ck-0001")["manifest"]) is None


def test_a_retry_whose_manifest_differs_is_refused_and_commits_nothing() -> None:
    inner = _storage()
    with pytest.raises(CheckpointError):
        publish_checkpoint(
            FaultingStorage(inner, after_create="/manifest.json"),
            _draft(),
            expected_attempt=1,
            expected_version=4,
        )
    changed = dataclasses.replace(
        _draft(),
        workspace_entries=(
            WorkspaceEntry(path="src/other.py", kind="file", executable=False),
        ),
    )
    with pytest.raises(CheckpointError) as info:
        publish_checkpoint(inner, changed, expected_attempt=1, expected_version=4)
    assert info.value.code is CheckpointErrorCode.INTEGRITY_FAILED
    assert inner.read_pointer(RUN_ID) is None
    assert inner.read_marker(_names("ck-0001")["manifest"]) is None


def test_a_new_checkpoint_id_after_a_failure_selects_only_its_own_marker() -> None:
    inner = _storage()
    with pytest.raises(CheckpointError):
        publish_checkpoint(
            FaultingStorage(inner, after_create="/harness.bin"),
            _draft("ck-0001"),
            expected_attempt=1,
            expected_version=4,
        )
    marker = publish_checkpoint(
        inner, _draft("ck-0002"), expected_attempt=1, expected_version=4
    )
    selected = _select(inner.reopen())
    assert selected.marker == marker
    assert selected.marker.manifest.object_name == _names("ck-0002")["manifest"]


# --- Component size limits before any upload (DPES-897 decision 2) ------------------------------


def test_an_oversize_harness_is_refused_before_any_upload() -> None:
    inner = _storage()
    storage = FaultingStorage(inner)
    with pytest.raises(CheckpointError) as info:
        publish_checkpoint(
            storage,
            _draft(harness=b"\0" * (MAX_CHECKPOINT_BYTES + 1)),
            expected_attempt=1,
            expected_version=4,
        )
    assert info.value.code is CheckpointErrorCode.LIMIT_EXCEEDED
    assert storage.events == []
    assert inner.object_names() == []


def test_an_oversize_workspace_is_refused_before_any_upload(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        checkpoint, "MAX_WORKSPACE_CHECKPOINT_BYTES", len(WORKSPACE) - 1
    )
    inner = _storage()
    storage = FaultingStorage(inner)
    with pytest.raises(CheckpointError) as info:
        publish_checkpoint(storage, _draft(), expected_attempt=1, expected_version=4)
    assert info.value.code is CheckpointErrorCode.LIMIT_EXCEEDED
    assert storage.events == []
    assert inner.object_names() == []


# --- Result retry and body cap (DPES-897 decisions 1 and 4) -------------------------------------


def test_a_retried_result_adopts_the_stored_body_after_a_failed_commit() -> None:
    inner = _storage()
    body = b'{"outcome":"succeeded"}'
    with pytest.raises(StorageFailure):
        publish_result(
            FaultingStorage(inner, after_transaction_body=True),
            _result(),
            body=body,
            now=NOW,
        )
    updated = publish_result(inner, _result(), body=body, now=NOW)
    assert updated.state is RunState.SUCCEEDED
    assert inner.object_names() == [RESULT_NAME]
    assert inner.read_object(RESULT_NAME) == body


def test_a_retried_result_with_other_bytes_is_refused_and_run_stays_open() -> None:
    inner = _storage()
    with pytest.raises(StorageFailure):
        publish_result(
            FaultingStorage(inner, after_transaction_body=True),
            _result(),
            body=b"first",
            now=NOW,
        )
    with pytest.raises(ProtocolError) as info:
        publish_result(inner, _result(), body=b"second", now=NOW)
    assert info.value.code is ErrorCode.CHECKSUM_MISMATCH
    assert inner.object_names() == [RESULT_NAME]
    assert inner.read_object(RESULT_NAME) == b"first"
    assert _stored(inner).state is RunState.RUNNING


def test_a_result_body_over_one_mebibyte_is_refused_before_any_write() -> None:
    inner = _storage()
    storage = FaultingStorage(inner)
    with pytest.raises(ProtocolError) as info:
        publish_result(storage, _result(), body=b"x" * (MAX_RESULT_BYTES + 1), now=NOW)
    assert info.value.code is ErrorCode.INVALID_REQUEST
    assert storage.events == []
    assert inner.object_names() == []


# --- Layout owner (DPES-897 decision 3) ---------------------------------------------------------


def test_checkpoint_components_share_one_directory_per_checkpoint_id() -> None:
    assert checkpoint_object_names(RUN_ID, 1, "ck-0001") == _names("ck-0001")
    with pytest.raises(ValueError, match="checkpoint_id"):
        checkpoint_object_names(RUN_ID, 1, "ck-1")


def test_replaying_an_older_committed_checkpoint_does_not_regress_the_selection() -> None:
    storage = _storage()
    first = publish_checkpoint(storage, _draft("ck-0001"), expected_attempt=1, expected_version=4)
    latest = publish_checkpoint(
        storage, _draft("ck-0002"), expected_attempt=1, expected_version=4
    )

    replay = publish_checkpoint(
        storage.reopen(), _draft("ck-0001"), expected_attempt=1, expected_version=4
    )

    assert replay == first
    assert _select(storage.reopen()).marker == latest
    assert _stored(storage).version == 4


def test_a_conflicting_committed_marker_does_not_change_the_current_selection() -> None:
    storage = _storage()
    first = publish_checkpoint(storage, _draft("ck-0001"), expected_attempt=1, expected_version=4)
    latest = publish_checkpoint(
        storage, _draft("ck-0002"), expected_attempt=1, expected_version=4
    )
    with storage.transaction() as txn:
        txn.put_marker(first.model_copy(update={"fence": 2}))

    with pytest.raises(CheckpointError) as refused:
        publish_checkpoint(storage, _draft("ck-0001"), expected_attempt=1, expected_version=4)

    assert refused.value.code is CheckpointErrorCode.INTEGRITY_FAILED
    assert _select(storage).marker == latest


def test_concurrent_commit_after_marker_read_cannot_overwrite_a_later_selection() -> None:
    class InterleavedStorage(InMemoryStorage):
        after_body: Callable[[], None] | None = None

        @contextmanager
        def transaction(self):
            with super().transaction() as txn:
                yield txn
                hook = self.after_body
                self.after_body = None
                if hook is not None:
                    hook()

    storage = InterleavedStorage()
    storage.seed_run(_current())
    committed: list[CommitMarker] = []

    def commit_first_and_then_latest() -> None:
        publish_checkpoint(storage, _draft("ck-0001"), expected_attempt=1, expected_version=4)
        committed.append(publish_checkpoint(
            storage, _draft("ck-0002"), expected_attempt=1, expected_version=4
        ))

    storage.after_body = commit_first_and_then_latest
    with pytest.raises(CheckpointError) as refused:
        publish_checkpoint(storage, _draft("ck-0001"), expected_attempt=1, expected_version=4)

    assert refused.value.code is CheckpointErrorCode.COMMIT_FAILED
    assert _select(storage.reopen()).marker == committed[0]
    replay = publish_checkpoint(storage, _draft("ck-0001"), expected_attempt=1, expected_version=4)
    assert replay.manifest.object_name == _names("ck-0001")["manifest"]
    assert _select(storage.reopen()).marker == committed[0]


def _per_checkpoint_bytes() -> int:
    return len(HARNESS) + len(WORKSPACE)


def test_publish_refuses_a_checkpoint_past_the_run_budget_before_any_object_is_created(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        checkpoint, "MAX_RUN_CHECKPOINT_BYTES", _per_checkpoint_bytes() + 5
    )
    storage = _storage()
    publish_checkpoint(
        storage, _draft("ck-0001"), expected_attempt=1, expected_version=4
    )
    objects_before = storage.object_names()

    with pytest.raises(CheckpointError) as raised:
        publish_checkpoint(
            storage, _draft("ck-0002"), expected_attempt=1, expected_version=4
        )

    assert raised.value.code is CheckpointErrorCode.LIMIT_EXCEEDED
    assert storage.object_names() == objects_before
    assert storage.read_pointer(RUN_ID) == _names("ck-0001")["manifest"]


def test_a_run_may_spend_its_budget_exactly(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        checkpoint, "MAX_RUN_CHECKPOINT_BYTES", 2 * _per_checkpoint_bytes()
    )
    storage = _storage()
    publish_checkpoint(
        storage, _draft("ck-0001"), expected_attempt=1, expected_version=4
    )
    publish_checkpoint(
        storage, _draft("ck-0002"), expected_attempt=1, expected_version=4
    )

    with pytest.raises(CheckpointError) as raised:
        publish_checkpoint(
            storage, _draft("ck-0003"), expected_attempt=1, expected_version=4
        )

    assert raised.value.code is CheckpointErrorCode.LIMIT_EXCEEDED
    assert storage.read_pointer(RUN_ID) == _names("ck-0002")["manifest"]


@pytest.mark.parametrize(
    "partial", ["/harness.bin", "/workspace.tar.gz", "/manifest.json"]
)
def test_a_retry_that_adopts_its_leftover_components_does_not_count_them_twice(
    monkeypatch: pytest.MonkeyPatch, partial: str
) -> None:
    monkeypatch.setattr(checkpoint, "MAX_RUN_CHECKPOINT_BYTES", _per_checkpoint_bytes())
    inner = _storage()
    with pytest.raises(CheckpointError):
        publish_checkpoint(
            FaultingStorage(inner, after_create=partial),
            _draft("ck-0001"),
            expected_attempt=1,
            expected_version=4,
        )

    marker = publish_checkpoint(
        inner, _draft("ck-0001"), expected_attempt=1, expected_version=4
    )

    assert _select(inner.reopen()).marker == marker


def test_replaying_a_committed_checkpoint_at_the_budget_limit_is_not_refused(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(checkpoint, "MAX_RUN_CHECKPOINT_BYTES", _per_checkpoint_bytes())
    storage = _storage()
    marker = publish_checkpoint(
        storage, _draft("ck-0001"), expected_attempt=1, expected_version=4
    )

    replay = publish_checkpoint(
        storage, _draft("ck-0001"), expected_attempt=1, expected_version=4
    )

    assert replay == marker


def test_a_failed_publish_frees_nothing_for_a_new_checkpoint_id(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        checkpoint, "MAX_RUN_CHECKPOINT_BYTES", 2 * _per_checkpoint_bytes() - 1
    )
    inner = _storage()
    with pytest.raises(CheckpointError) as failed:
        publish_checkpoint(
            FaultingStorage(inner, after_create="/manifest.json"),
            _draft("ck-0001"),
            expected_attempt=1,
            expected_version=4,
        )
    assert failed.value.code is CheckpointErrorCode.COMMIT_FAILED

    with pytest.raises(CheckpointError) as refused:
        publish_checkpoint(
            inner, _draft("ck-0002"), expected_attempt=1, expected_version=4
        )

    assert refused.value.code is CheckpointErrorCode.LIMIT_EXCEEDED
    assert inner.read_pointer(RUN_ID) is None


def test_overlapping_checkpoint_ids_cannot_spend_the_same_run_budget(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(checkpoint, "MAX_RUN_CHECKPOINT_BYTES", 30)
    inner = _storage()
    first = dataclasses.replace(_draft("ck-0001"), harness_bytes=b"h" * 10, workspace_bytes=b"w" * 10)
    second = dataclasses.replace(_draft("ck-0002"), harness_bytes=b"h" * 10, workspace_bytes=b"w" * 10)
    overlapping = FaultingStorage(inner.reopen())
    refused: list[CheckpointErrorCode] = []

    def publish_second_after_first_harness(name: str) -> None:
        if name == _names("ck-0001")["harness"]:
            with pytest.raises(CheckpointError) as info:
                publish_checkpoint(overlapping, second, expected_attempt=1, expected_version=4)
            refused.append(info.value.code)

    marker = publish_checkpoint(
        FaultingStorage(inner, on_create=publish_second_after_first_harness), first,
        expected_attempt=1, expected_version=4,
    )

    assert refused == [CheckpointErrorCode.LIMIT_EXCEEDED]
    assert overlapping.events == []
    assert inner.checkpoint_component_bytes(RUN_ID) == 20
    assert _select(inner.reopen()).marker == marker
    assert all("ck-0002" not in name for name in inner.object_names())


def test_pending_components_stay_reserved_after_a_partial_failure_and_reopen(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(checkpoint, "MAX_RUN_CHECKPOINT_BYTES", 30)
    inner = _storage()
    first = dataclasses.replace(_draft("ck-0001"), harness_bytes=b"h" * 10, workspace_bytes=b"w" * 10)
    second = dataclasses.replace(_draft("ck-0002"), harness_bytes=b"h" * 10, workspace_bytes=b"w" * 10)
    with pytest.raises(CheckpointError):
        publish_checkpoint(
            FaultingStorage(inner, after_create="/harness.bin"), first,
            expected_attempt=1, expected_version=4,
        )
    assert inner.checkpoint_component_bytes(RUN_ID) == 10
    reopened = inner.reopen()
    with pytest.raises(CheckpointError) as refused:
        publish_checkpoint(reopened, second, expected_attempt=1, expected_version=4)
    assert refused.value.code is CheckpointErrorCode.LIMIT_EXCEEDED
    marker = publish_checkpoint(reopened, first, expected_attempt=1, expected_version=4)
    assert _select(inner.reopen()).marker == marker
    assert inner.checkpoint_component_bytes(RUN_ID) == 20
