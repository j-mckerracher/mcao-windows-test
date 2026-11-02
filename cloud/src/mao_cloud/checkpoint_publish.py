"""Checkpoint and result publication over the Storage port (DPES-869 sections 15.4 and 15.5).

Checkpoint order: check identity against the controller's record, check the component sizes and the
run budget, create or adopt the harness and workspace components, create or adopt the manifest that
names them, read each back and verify it, then commit the marker and the pointer in one transaction
that re-checks identity. Any failure before that commit leaves the previous selection in place.
Publication never bumps the run version.

A retry after a partial upload reuses the same checkpoint id: a component already stored with
exactly these bytes is adopted, and different bytes under that id are refused. A changed draft needs
a new checkpoint id. Nothing is committed until every component reads back. Replaying a committed
checkpoint returns its marker without changing the currently selected checkpoint.

The storage authority reserves the run budget atomically before any component upload. Stored
components and pending reservations count once per name, including failed publishes, so failure
frees nothing. A retry reuses its name's allocation. Manifests are not counted.

Results: check identity, check the body size, then create or adopt the create-only result object
(the same rule), and accept the result in a transaction.
"""

from dataclasses import dataclass
from datetime import datetime

from mao_cloud import checkpoint

from mao_cloud.checkpoint import (
    CheckpointError,
    CheckpointErrorCode,
    CheckpointManifest,
    CheckpointSelection,
    CommitMarker,
    ObjectRef,
    ObservedObject,
    WorkspaceEntry,
    check_component_sizes,
    parse_manifest,
    select_checkpoint,
    verify_component,
)
from mao_cloud.protocol import (
    MAX_RESULT_BYTES,
    ClaimId,
    ErrorCode,
    GitSha,
    Harness,
    ImageDigest,
    ObjectKind,
    ProtocolError,
    RunId,
    RunRecord,
    RunResultDocument,
    Sha256Hex,
    accept_result,
    check_selection_publishable,
    checkpoint_object_names,
    object_name,
    sha256_hex,
)
from mao_cloud.storage import Storage, StorageConflict, StorageFailure, Transaction


@dataclass(frozen=True)
class CheckpointDraft:
    run_id: RunId
    attempt: int
    fence: int
    claim_id: ClaimId
    checkpoint_id: str
    base_sha: GitSha
    source_digest: Sha256Hex
    harness: Harness
    harness_version: str
    image_digest: ImageDigest
    harness_bytes: bytes
    workspace_bytes: bytes
    workspace_entries: tuple[WorkspaceEntry, ...]


def _stored_run(storage: Storage, run_id: str) -> RunRecord:
    run = storage.read_run(run_id)
    if run is None:
        raise ProtocolError(ErrorCode.NOT_FOUND, "run is not stored")
    return run


def _txn_run(txn: Transaction, run_id: str) -> RunRecord:
    run = txn.get_run(run_id)
    if run is None:
        raise ProtocolError(ErrorCode.NOT_FOUND, "run is not stored")
    return run


def _check_identity(
    record: RunRecord,
    draft: CheckpointDraft,
    *,
    expected_attempt: int,
    expected_version: int,
) -> None:
    check_selection_publishable(
        record,
        attempt=draft.attempt,
        fence=draft.fence,
        claim_id=draft.claim_id,
        expected_attempt=expected_attempt,
        expected_version=expected_version,
    )


def _ref(name: str, data: bytes, generation: int) -> ObjectRef:
    return ObjectRef(
        object_name=name,
        generation=generation,
        sha256=sha256_hex(data),
        size_bytes=len(data),
    )


def _observe(storage: Storage, observed: dict[str, ObservedObject], name: str) -> None:
    head = storage.head(name)
    if head is not None:
        observed[name] = ObservedObject(
            generation=head.generation,
            size_bytes=head.size_bytes,
            sha256=head.sha256,
        )


def _matching_generation(storage: Storage, name: str, data: bytes) -> int | None:
    """The generation of a stored object that holds exactly these bytes, or None."""
    head = storage.head(name)
    if head is None or head.sha256 != sha256_hex(data) or head.size_bytes != len(data):
        return None
    return head.generation


def _create_or_adopt(storage: Storage, name: str, data: bytes) -> int:
    """Create a checkpoint component, or adopt an identical one from an earlier try at this id."""
    try:
        return storage.create_object(name, data)
    except StorageConflict:
        generation = _matching_generation(storage, name, data)
        if generation is None:
            raise CheckpointError(
                CheckpointErrorCode.INTEGRITY_FAILED,
                "a checkpoint component exists with different bytes; "
                "retry with a new checkpoint id",
            ) from None
        return generation


def publish_checkpoint(
    storage: Storage,
    draft: CheckpointDraft,
    *,
    expected_attempt: int,
    expected_version: int,
) -> CommitMarker:
    _check_identity(
        _stored_run(storage, draft.run_id),
        draft,
        expected_attempt=expected_attempt,
        expected_version=expected_version,
    )
    check_component_sizes(len(draft.harness_bytes), len(draft.workspace_bytes))
    names = checkpoint_object_names(draft.run_id, draft.attempt, draft.checkpoint_id)
    try:
        admitted = storage.reserve_checkpoint_components(
            draft.run_id,
            {names["harness"]: len(draft.harness_bytes), names["workspace"]: len(draft.workspace_bytes)},
            max_bytes=checkpoint.MAX_RUN_CHECKPOINT_BYTES,
        )
    except StorageConflict:
        raise CheckpointError(
            CheckpointErrorCode.INTEGRITY_FAILED,
            "a checkpoint component was reserved with a different size; use a new checkpoint id",
        ) from None
    except StorageFailure:
        raise CheckpointError(
            CheckpointErrorCode.COMMIT_FAILED, "checkpoint budget admission did not complete"
        ) from None
    if not admitted:
        raise CheckpointError(
            CheckpointErrorCode.LIMIT_EXCEEDED, "the run checkpoint byte budget is exhausted"
        )
    try:
        harness_ref = _ref(
            names["harness"],
            draft.harness_bytes,
            _create_or_adopt(storage, names["harness"], draft.harness_bytes),
        )
        workspace_ref = _ref(
            names["workspace"],
            draft.workspace_bytes,
            _create_or_adopt(storage, names["workspace"], draft.workspace_bytes),
        )
        manifest = CheckpointManifest(
            schema_version=1,
            run_id=draft.run_id,
            attempt=draft.attempt,
            fence=draft.fence,
            claim_id=draft.claim_id,
            base_sha=draft.base_sha,
            source_digest=draft.source_digest,
            harness=draft.harness,
            harness_version=draft.harness_version,
            image_digest=draft.image_digest,
            harness_state=harness_ref,
            workspace_state=workspace_ref,
            workspace_entries=list(draft.workspace_entries),
        )
        manifest_bytes = manifest.model_dump_json().encode()
        manifest_ref = _ref(
            names["manifest"],
            manifest_bytes,
            _create_or_adopt(storage, names["manifest"], manifest_bytes),
        )
        for ref in (harness_ref, workspace_ref, manifest_ref):
            observed: dict[str, ObservedObject] = {}
            _observe(storage, observed, ref.object_name)
            verify_component(ref, observed.get(ref.object_name))
        marker = CommitMarker(
            run_id=draft.run_id,
            attempt=draft.attempt,
            fence=draft.fence,
            claim_id=draft.claim_id,
            manifest=manifest_ref,
            committed=True,
        )
        with storage.transaction() as txn:
            _check_identity(
                _txn_run(txn, draft.run_id),
                draft,
                expected_attempt=expected_attempt,
                expected_version=expected_version,
            )
            existing = txn.get_marker(names["manifest"])
            if existing is not None:
                if existing != marker:
                    raise CheckpointError(
                        CheckpointErrorCode.INTEGRITY_FAILED,
                        "the committed checkpoint marker does not match this draft",
                    )
                return existing
            txn.put_marker(marker)
            txn.put_pointer(draft.run_id, names["manifest"])
    except StorageFailure:
        raise CheckpointError(
            CheckpointErrorCode.COMMIT_FAILED, "checkpoint commit did not complete"
        ) from None
    return marker


def read_selection(
    storage: Storage,
    run: RunRecord,
    *,
    harness_version: str,
    image_digest: str,
    expected_attempt: int,
    expected_version: int,
) -> CheckpointSelection:
    manifest_name = storage.read_pointer(run.run_id)
    marker = None if manifest_name is None else storage.read_marker(manifest_name)
    manifest_bytes = (
        None if manifest_name is None else storage.read_object(manifest_name)
    )
    observed: dict[str, ObservedObject] = {}
    if manifest_name is not None:
        _observe(storage, observed, manifest_name)
        if manifest_bytes is not None:
            try:
                parsed = parse_manifest(manifest_bytes)
            except CheckpointError:
                parsed = None
            if parsed is not None:
                _observe(storage, observed, parsed.harness_state.object_name)
                _observe(storage, observed, parsed.workspace_state.object_name)
    return select_checkpoint(
        run,
        marker=marker,
        manifest_bytes=manifest_bytes,
        observed=observed,
        harness_version=harness_version,
        image_digest=image_digest,
        expected_attempt=expected_attempt,
        expected_version=expected_version,
    )


def publish_result(
    storage: Storage,
    document: RunResultDocument,
    *,
    body: bytes,
    now: datetime,
) -> RunRecord:
    accept_result(_stored_run(storage, document.run_id), document, now=now)
    if len(body) > MAX_RESULT_BYTES:
        raise ProtocolError(
            ErrorCode.INVALID_REQUEST,
            f"result body exceeds the {MAX_RESULT_BYTES}-byte limit",
        )
    result_name = object_name(
        ObjectKind.RESULT, run_id=document.run_id, attempt=document.attempt
    )
    try:
        storage.create_object(result_name, body)
    except StorageConflict:
        if _matching_generation(storage, result_name, body) is None:
            raise ProtocolError(
                ErrorCode.CHECKSUM_MISMATCH,
                "a result is already stored with different bytes",
            ) from None
    with storage.transaction() as txn:
        record = _txn_run(txn, document.run_id)
        updated = accept_result(record, document, now=now)
        txn.put_run(updated, expected_version=record.version)
    return updated
