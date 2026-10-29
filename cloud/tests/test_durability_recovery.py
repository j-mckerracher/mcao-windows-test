"""Durability and recovery cases for DPES-871, against the in-memory and faulting Storage fakes.

Every result here is FAKE. reopen() stands in for a controller or execution restart: the reopened
handle shares the committed backing and holds no local workspace, so restore is proven from
committed bytes only. Real GCS generations and atomicity, a real Cloud Run execution, SIGKILL, OOM,
and submitter disconnect are stated gaps in the PR body, not proven here.
"""

from datetime import timedelta

import pytest

from fakes import EPOCH, IMAGE_DIGEST, RUN_ID, run_record
from mao_cloud.checkpoint import (
    CheckpointError,
    CheckpointErrorCode,
    CheckpointSelection,
    WorkspaceEntry,
)
from mao_cloud.checkpoint_publish import (
    CheckpointDraft,
    publish_checkpoint,
    publish_result,
    read_selection,
)
from mao_cloud.protocol import (
    ErrorCode,
    ObjectKind,
    ProtocolError,
    RunRecord,
    RunResultDocument,
    RunState,
    object_name,
)
from mao_cloud.storage import StorageFailure
from storage_fakes import FaultingStorage, InMemoryStorage

CLAIM_ID = "claim_" + "e" * 32
NEW_CLAIM_ID = "claim_" + "f" * 32
BASE_SHA = "1" * 40
SOURCE_DIGEST = "a" * 64
HARNESS_VERSION = "1.3.0"
NOW = EPOCH + timedelta(seconds=1)
HARNESS_V1 = b"harness-state-v1"
WORKSPACE_V1 = b"workspace-archive-v1"
HARNESS_V2 = b"harness-state-v2"
WORKSPACE_V2 = b"workspace-archive-v2"
RESULT_BODY = b'{"outcome":"succeeded"}'
RESULT_NAME = object_name(ObjectKind.RESULT, run_id=RUN_ID, attempt=1)
ENTRIES = (WorkspaceEntry(path="src/app.py", kind="file", executable=False),)


def _run(**overrides: object) -> RunRecord:
    values: dict[str, object] = {
        "state": RunState.RUNNING,
        "claim_id": CLAIM_ID,
        "attempt": 1,
        "fence": 1,
        "version": 4,
        "last_seen_at": EPOCH,
    }
    values.update(overrides)
    return run_record(**values)


def _storage() -> InMemoryStorage:
    storage = InMemoryStorage()
    storage.seed_run(_run())
    return storage


def _draft(
    checkpoint_id: str = "ck-0001",
    *,
    attempt: int = 1,
    fence: int = 1,
    claim_id: str = CLAIM_ID,
    harness: bytes = HARNESS_V1,
    workspace: bytes = WORKSPACE_V1,
) -> CheckpointDraft:
    return CheckpointDraft(
        run_id=RUN_ID,
        attempt=attempt,
        fence=fence,
        claim_id=claim_id,
        checkpoint_id=checkpoint_id,
        base_sha=BASE_SHA,
        source_digest=SOURCE_DIGEST,
        harness="pi",
        harness_version=HARNESS_VERSION,
        image_digest=IMAGE_DIGEST,
        harness_bytes=harness,
        workspace_bytes=workspace,
        workspace_entries=ENTRIES,
    )


def _names(checkpoint_id: str, attempt: int = 1) -> dict[str, str]:
    base = f"runs/{RUN_ID}/attempts/{attempt:06d}/checkpoints/{checkpoint_id}"
    return {
        "harness": f"{base}/harness.bin",
        "workspace": f"{base}/workspace.tar.gz",
        "manifest": f"{base}/manifest.json",
    }


def _select(
    storage: InMemoryStorage, *, attempt: int = 1, version: int = 4
) -> CheckpointSelection:
    run = storage.read_run(RUN_ID)
    assert run is not None
    return read_selection(
        storage,
        run,
        harness_version=HARNESS_VERSION,
        image_digest=IMAGE_DIGEST,
        expected_attempt=attempt,
        expected_version=version,
    )


def _restored_bytes(
    storage: InMemoryStorage, selection: CheckpointSelection
) -> tuple[bytes, bytes]:
    manifest = selection.manifest
    harness = storage.read_object(manifest.harness_state.object_name)
    workspace = storage.read_object(manifest.workspace_state.object_name)
    assert harness is not None and workspace is not None
    return harness, workspace


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


# AC1 (fake level): a restarted handle restores the committed pair from storage alone.


def test_a_reopened_handle_restores_the_committed_bytes_and_entries() -> None:
    storage = _storage()
    marker = publish_checkpoint(
        storage, _draft(), expected_attempt=1, expected_version=4
    )
    restarted = storage.reopen()
    selection = _select(restarted)
    assert selection.marker == marker  # type: ignore[attr-defined]
    assert _restored_bytes(restarted, selection) == (HARNESS_V1, WORKSPACE_V1)
    assert list(selection.manifest.workspace_entries) == list(ENTRIES)  # type: ignore[attr-defined]


def test_a_kill_during_the_next_publish_keeps_the_committed_pair_intact() -> None:
    inner = _storage()
    prior = publish_checkpoint(
        inner, _draft("ck-0001"), expected_attempt=1, expected_version=4
    )
    with pytest.raises(CheckpointError):
        publish_checkpoint(
            FaultingStorage(inner, after_create="/workspace.tar.gz"),
            _draft("ck-0002", harness=HARNESS_V2, workspace=WORKSPACE_V2),
            expected_attempt=1,
            expected_version=4,
        )
    restarted = inner.reopen()
    selection = _select(restarted)
    assert selection.marker == prior  # type: ignore[attr-defined]
    assert _restored_bytes(restarted, selection) == (HARNESS_V1, WORKSPACE_V1)
    assert restarted.read_marker(_names("ck-0002")["manifest"]) is None


# AC2 (fake level): partial uploads and damaged committed objects are never restored as success.


def test_a_retry_after_a_partial_upload_selects_only_the_retry() -> None:
    inner = _storage()
    with pytest.raises(CheckpointError):
        publish_checkpoint(
            FaultingStorage(inner, after_create="/harness.bin"),
            _draft("ck-0001"),
            expected_attempt=1,
            expected_version=4,
        )
    restarted = inner.reopen()
    with pytest.raises(CheckpointError) as nothing:
        _select(restarted)
    assert nothing.value.code is CheckpointErrorCode.MISSING
    retry = publish_checkpoint(
        restarted,
        _draft("ck-0002", harness=HARNESS_V2, workspace=WORKSPACE_V2),
        expected_attempt=1,
        expected_version=4,
    )
    selection = _select(restarted)
    assert selection.marker == retry  # type: ignore[attr-defined]
    assert _restored_bytes(restarted, selection) == (HARNESS_V2, WORKSPACE_V2)
    assert _names("ck-0001")["harness"] in restarted.object_names()


@pytest.mark.parametrize("component", ["harness", "workspace"])
@pytest.mark.parametrize(
    "damage", ["flipped", "truncated", "appended"], ids=lambda d: d
)
def test_a_committed_component_whose_bytes_changed_is_refused_on_restore(
    component: str, damage: str
) -> None:
    inner = _storage()
    publish_checkpoint(inner, _draft(), expected_attempt=1, expected_version=4)
    name = _names("ck-0001")[component]
    original = inner.read_object(name)
    assert original is not None
    damaged = {
        "flipped": b"X" + original[1:],
        "truncated": original[:-1],
        "appended": original + b"\n",
    }[damage]
    inner.corrupt_object(name, damaged)
    with pytest.raises(CheckpointError) as info:
        _select(inner.reopen())
    assert info.value.code is CheckpointErrorCode.INTEGRITY_FAILED


@pytest.mark.parametrize("component", ["harness", "workspace"])
def test_a_committed_component_removed_after_commit_is_reported_missing(
    component: str,
) -> None:
    inner = _storage()
    publish_checkpoint(inner, _draft(), expected_attempt=1, expected_version=4)
    inner.remove_object(_names("ck-0001")[component])
    with pytest.raises(CheckpointError) as info:
        _select(inner.reopen())
    assert info.value.code is CheckpointErrorCode.MISSING


# AC4 (fake level): fence and attempt changes across a restart refuse the old writer.


def test_a_stale_attempt_publisher_after_restart_cannot_replace_the_new_selection() -> (
    None
):
    inner = _storage()
    publish_checkpoint(inner, _draft("ck-0001"), expected_attempt=1, expected_version=4)
    inner.seed_run(_run(attempt=2, fence=2, claim_id=NEW_CLAIM_ID, version=5))
    newer = publish_checkpoint(
        inner,
        _draft("ck-0001", attempt=2, fence=2, claim_id=NEW_CLAIM_ID),
        expected_attempt=2,
        expected_version=5,
    )
    assert _select(inner, attempt=2, version=5).marker == newer  # type: ignore[attr-defined]
    stale = FaultingStorage(inner.reopen())
    with pytest.raises(ProtocolError) as info:
        publish_checkpoint(
            stale, _draft("ck-0002"), expected_attempt=1, expected_version=4
        )
    assert info.value.code is ErrorCode.STALE_ATTEMPT
    assert stale.events == []
    assert _select(inner, attempt=2, version=5).marker == newer  # type: ignore[attr-defined]


def test_a_stale_fence_publisher_after_restart_is_refused_before_any_upload() -> None:
    inner = _storage()
    inner.seed_run(_run(fence=2, version=5))
    stale = FaultingStorage(inner)
    with pytest.raises(ProtocolError) as info:
        publish_checkpoint(
            stale, _draft(fence=1), expected_attempt=1, expected_version=5
        )
    assert info.value.code is ErrorCode.STALE_ATTEMPT
    assert stale.events == []
    assert inner.object_names() == []


# AC2 and T2 (defect exposed by the fakes): result publication must survive a fault.


@pytest.mark.parametrize(
    "fault",
    [{"after_create": "/result.json"}, {"after_transaction_body": True}],
    ids=["after-object-write", "after-acceptance-body"],
)
def test_a_result_retried_after_a_fault_is_accepted_once(
    fault: dict[str, object],
) -> None:
    inner = _storage()
    with pytest.raises(StorageFailure):
        publish_result(
            FaultingStorage(inner, **fault),  # type: ignore[arg-type]
            _result(),
            body=RESULT_BODY,
            now=NOW,
        )
    restarted = inner.reopen()
    assert restarted.read_run(RUN_ID).state is RunState.RUNNING  # type: ignore[union-attr]
    updated = publish_result(restarted, _result(), body=RESULT_BODY, now=NOW)
    assert updated.state is RunState.SUCCEEDED
    assert restarted.read_object(RESULT_NAME) == RESULT_BODY
    assert restarted.object_names() == [RESULT_NAME]


def test_a_retry_with_a_different_result_body_after_a_fault_changes_nothing() -> None:
    inner = _storage()
    with pytest.raises(StorageFailure):
        publish_result(
            FaultingStorage(inner, after_transaction_body=True),
            _result(),
            body=RESULT_BODY,
            now=NOW,
        )
    restarted = inner.reopen()
    with pytest.raises(ProtocolError) as info:
        publish_result(restarted, _result(), body=b'{"outcome":"failed"}', now=NOW)
    assert info.value.code is ErrorCode.CHECKSUM_MISMATCH
    assert restarted.read_run(RUN_ID).state is RunState.RUNNING  # type: ignore[union-attr]
    assert restarted.read_object(RESULT_NAME) == RESULT_BODY
