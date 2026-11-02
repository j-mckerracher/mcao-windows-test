"""Checkpoint schema v1 and the pre-launch restore gate (DPES-869 Q2, pure logic).

This module defines the immutable checkpoint manifest, the commit marker written with the
Firestore pointer, the Q2 limits, the six typed error codes, the selection gate that decides
which committed checkpoint a resume may name, and the pre-launch gate that must pass before any
agent or repository code runs on restored state. It performs no I/O: callers pass in the
observed storage identities. Storage reads and the two-step commit are separate work.
"""

import json
import posixpath
import re
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from enum import StrEnum
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field, ValidationError, model_validator

from mao_cloud.protocol import (
    MAX_CHECKPOINT_BYTES,
    ClaimId,
    GitSha,
    Harness,
    ImageDigest,
    RunId,
    RunRecord,
    Sha256Hex,
    check_selection_publishable,
    sha256_hex,
)

CHECKPOINT_SCHEMA_VERSION = 1
MAX_HARNESS_STATE_BYTES = MAX_CHECKPOINT_BYTES
MAX_WORKSPACE_CHECKPOINT_BYTES = 1024**3
MAX_RUN_CHECKPOINT_BYTES = 2 * 1024**3

_CONTROL_CHARACTER = re.compile(r"[\x00-\x1f\x7f]")


class CheckpointErrorCode(StrEnum):
    INCOMPATIBLE = "checkpoint_incompatible"
    INTEGRITY_FAILED = "checkpoint_integrity_failed"
    MISSING = "checkpoint_missing"
    LIMIT_EXCEEDED = "checkpoint_limit_exceeded"
    COMMIT_FAILED = "checkpoint_commit_failed"
    RESTORE_UNSAFE_PATH = "restore_unsafe_path"


class CheckpointError(Exception):
    """Typed recovery failure. Messages never echo workspace paths, contents, or URLs."""

    def __init__(self, code: CheckpointErrorCode, message: str) -> None:
        self.code = code
        super().__init__(f"{code.value}: {message}")


class _CheckpointWire(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)


def _under_run_prefix(object_name: str, run_id: str) -> bool:
    if not object_name.startswith(f"runs/{run_id}/"):
        return False
    return all(segment not in ("", ".", "..") for segment in object_name.split("/"))


class ObjectRef(_CheckpointWire):
    """An immutable GCS object identified by generation, size, and SHA-256."""

    object_name: str = Field(min_length=1, max_length=1024)
    generation: int = Field(ge=1)
    sha256: Sha256Hex
    size_bytes: int = Field(ge=0)


class WorkspaceEntry(_CheckpointWire):
    """One changed workspace path. Contents live in the workspace object, not here."""

    path: str = Field(max_length=4096)
    kind: Literal["file", "deleted", "symlink"]
    executable: bool

    @model_validator(mode="after")
    def _deleted_entry_has_no_mode(self) -> "WorkspaceEntry":
        if self.kind == "deleted" and self.executable:
            raise ValueError("a deleted entry cannot carry an executable bit")
        return self


class CheckpointManifest(_CheckpointWire):
    """Schema v1. Written as an immutable object before any pointer names it."""

    schema_version: Literal[1]
    run_id: RunId
    attempt: int = Field(ge=1)
    fence: int = Field(ge=1)
    claim_id: ClaimId
    base_sha: GitSha
    source_digest: Sha256Hex
    harness: Harness
    harness_version: str = Field(pattern=r"^[0-9]+\.[0-9]+\.[0-9]+$")
    image_digest: ImageDigest
    harness_state: ObjectRef
    workspace_state: ObjectRef
    workspace_entries: list[WorkspaceEntry] = Field(default_factory=list)

    @model_validator(mode="after")
    def _components_sit_under_run_prefix(self) -> "CheckpointManifest":
        for ref in (self.harness_state, self.workspace_state):
            if not _under_run_prefix(ref.object_name, self.run_id):
                raise ValueError("component object must sit under the run prefix")
        return self

    @model_validator(mode="after")
    def _workspace_paths_are_unique(self) -> "CheckpointManifest":
        paths = [entry.path for entry in self.workspace_entries]
        if len(paths) != len(set(paths)):
            raise ValueError("workspace entries must have unique paths")
        return self


class CommitMarker(_CheckpointWire):
    """Written in the same Firestore transaction as the pointer, after the manifest is durable."""

    run_id: RunId
    attempt: int = Field(ge=1)
    fence: int = Field(ge=1)
    claim_id: ClaimId
    manifest: ObjectRef
    committed: Literal[True]

    @model_validator(mode="after")
    def _manifest_sits_under_run_prefix(self) -> "CommitMarker":
        if not _under_run_prefix(self.manifest.object_name, self.run_id):
            raise ValueError("manifest object must sit under the run prefix")
        return self


@dataclass(frozen=True)
class ObservedObject:
    """Identity the storage layer observed when it read an object back."""

    generation: int
    size_bytes: int
    sha256: str


def _reject_constant(name: str) -> Any:
    raise ValueError(f"non-finite number {name} is not allowed")


def parse_manifest(data: bytes) -> CheckpointManifest:
    """Parse manifest bytes. Version is checked before the rest of the schema."""
    try:
        payload: Any = json.loads(data, parse_constant=_reject_constant)
    except ValueError as exc:
        raise CheckpointError(
            CheckpointErrorCode.INTEGRITY_FAILED, "manifest is not valid JSON"
        ) from exc
    if not isinstance(payload, dict):
        raise CheckpointError(
            CheckpointErrorCode.INTEGRITY_FAILED, "manifest is not a JSON object"
        )
    if payload.get("schema_version") != CHECKPOINT_SCHEMA_VERSION:
        raise CheckpointError(
            CheckpointErrorCode.INCOMPATIBLE, "unsupported checkpoint schema version"
        )
    try:
        return CheckpointManifest.model_validate(payload)
    except ValidationError:
        raise CheckpointError(
            CheckpointErrorCode.INTEGRITY_FAILED, "manifest failed schema validation"
        ) from None


def check_compatibility(
    manifest: CheckpointManifest, *, harness_version: str, image_digest: str
) -> None:
    if (
        manifest.harness_version != harness_version
        or manifest.image_digest != image_digest
    ):
        raise CheckpointError(
            CheckpointErrorCode.INCOMPATIBLE,
            "checkpoint was written by a different pinned harness or image",
        )


def check_component_sizes(harness_bytes: int, workspace_bytes: int) -> None:
    """Single owner of the per-component checkpoint limits, used before upload and on restore."""
    if harness_bytes > MAX_HARNESS_STATE_BYTES:
        raise CheckpointError(
            CheckpointErrorCode.LIMIT_EXCEEDED,
            "harness state exceeds its checkpoint limit",
        )
    if workspace_bytes > MAX_WORKSPACE_CHECKPOINT_BYTES:
        raise CheckpointError(
            CheckpointErrorCode.LIMIT_EXCEEDED, "workspace checkpoint exceeds its limit"
        )


def check_component_limits(manifest: CheckpointManifest) -> None:
    check_component_sizes(
        manifest.harness_state.size_bytes, manifest.workspace_state.size_bytes
    )


def check_run_budget(used_bytes: int, next_bytes: int) -> None:
    """Refuse a new checkpoint once the run's cumulative bytes would pass the per-run limit."""
    if used_bytes + next_bytes > MAX_RUN_CHECKPOINT_BYTES:
        raise CheckpointError(
            CheckpointErrorCode.LIMIT_EXCEEDED, "run checkpoint budget is exhausted"
        )


def _is_safe_relative_path(path: str) -> bool:
    if path.startswith("/") or "\\" in path or _CONTROL_CHARACTER.search(path):
        return False
    return all(segment not in ("", ".", "..") for segment in path.split("/"))


def check_restore_paths(manifest: CheckpointManifest) -> None:
    """Refuse symlink entries and any path that could escape the workspace on restore."""
    for entry in manifest.workspace_entries:
        if entry.kind == "symlink" or not _is_safe_relative_path(entry.path):
            raise CheckpointError(
                CheckpointErrorCode.RESTORE_UNSAFE_PATH,
                "workspace entry is a symlink or not a safe relative path",
            )


def verify_component(ref: ObjectRef, observed: ObservedObject | None) -> None:
    """Check a component read back from storage against its recorded identity."""
    if observed is None:
        raise CheckpointError(
            CheckpointErrorCode.MISSING, "checkpoint component is missing"
        )
    if (
        observed.generation != ref.generation
        or observed.size_bytes != ref.size_bytes
        or observed.sha256 != ref.sha256
    ):
        raise CheckpointError(
            CheckpointErrorCode.INTEGRITY_FAILED,
            "checkpoint component does not match its recorded generation, size, or checksum",
        )


def check_commit_matches(marker: CommitMarker, manifest: CheckpointManifest) -> None:
    if (marker.run_id, marker.attempt, marker.fence, marker.claim_id) != (
        manifest.run_id,
        manifest.attempt,
        manifest.fence,
        manifest.claim_id,
    ):
        raise CheckpointError(
            CheckpointErrorCode.INTEGRITY_FAILED,
            "commit marker does not name the manifest's run, attempt, fence, and claim",
        )


def validate_for_restore(
    data: bytes,
    *,
    run_id: str,
    harness_version: str,
    image_digest: str,
) -> CheckpointManifest:
    """Manifest-level gate: parse, run identity, pinned compatibility, limits, and paths."""
    manifest = parse_manifest(data)
    if manifest.run_id != run_id:
        raise CheckpointError(
            CheckpointErrorCode.INTEGRITY_FAILED,
            "checkpoint belongs to a different logical run",
        )
    check_compatibility(
        manifest, harness_version=harness_version, image_digest=image_digest
    )
    check_component_limits(manifest)
    check_restore_paths(manifest)
    return manifest


class PublicationBoundary(StrEnum):
    """Points where a publish can fault. Each one must leave no commit marker behind."""

    HARNESS_STATE_WRITTEN = "harness_state_written"
    WORKSPACE_STATE_WRITTEN = "workspace_state_written"
    MANIFEST_WRITTEN = "manifest_written"
    POINTER_TRANSACTION_FAILED = "pointer_transaction_failed"


PUBLICATION_BOUNDARIES: tuple[PublicationBoundary, ...] = tuple(PublicationBoundary)


@dataclass(frozen=True)
class CheckpointSelection:
    """A committed checkpoint a resume may name: its commit marker and the manifest it commits."""

    marker: CommitMarker
    manifest: CheckpointManifest


def _one_checkpoint_directory(
    manifest: CheckpointManifest, marker: CommitMarker
) -> bool:
    """Harness state, workspace state, and manifest must come from one checkpoint directory."""
    directory = posixpath.dirname(marker.manifest.object_name)
    return all(
        posixpath.dirname(ref.object_name) == directory
        for ref in (manifest.harness_state, manifest.workspace_state)
    )


def select_checkpoint(
    run: RunRecord,
    *,
    marker: CommitMarker | None,
    manifest_bytes: bytes | None,
    observed: Mapping[str, ObservedObject],
    harness_version: str,
    image_digest: str,
    expected_attempt: int,
    expected_version: int,
) -> CheckpointSelection:
    """Select a committed checkpoint for resume, or raise a typed error and select nothing.

    The caller keeps its previous selection when this raises. Nothing is selected unless the
    marker names the run's current attempt, fence, and claim under the caller's compare-and-set,
    the manifest bytes match the committed digest, and every object read back from storage
    matches its recorded generation, size, and checksum.
    """
    if marker is None or manifest_bytes is None:
        raise CheckpointError(
            CheckpointErrorCode.MISSING, "no committed checkpoint names this attempt"
        )
    if marker.run_id != run.run_id:
        raise CheckpointError(
            CheckpointErrorCode.INTEGRITY_FAILED,
            "commit marker belongs to a different logical run",
        )
    check_selection_publishable(
        run,
        attempt=marker.attempt,
        fence=marker.fence,
        claim_id=marker.claim_id,
        expected_attempt=expected_attempt,
        expected_version=expected_version,
    )
    if (
        sha256_hex(manifest_bytes) != marker.manifest.sha256
        or len(manifest_bytes) != marker.manifest.size_bytes
    ):
        raise CheckpointError(
            CheckpointErrorCode.INTEGRITY_FAILED,
            "manifest bytes do not match the committed digest",
        )
    verify_component(marker.manifest, observed.get(marker.manifest.object_name))
    manifest = validate_for_restore(
        manifest_bytes,
        run_id=run.run_id,
        harness_version=harness_version,
        image_digest=image_digest,
    )
    check_commit_matches(marker, manifest)
    if not _one_checkpoint_directory(manifest, marker):
        raise CheckpointError(
            CheckpointErrorCode.INTEGRITY_FAILED,
            "harness state, workspace state, and manifest come from different checkpoints",
        )
    for ref in (manifest.harness_state, manifest.workspace_state):
        verify_component(ref, observed.get(ref.object_name))
    return CheckpointSelection(marker=marker, manifest=manifest)


def admit_restore(
    data: bytes,
    *,
    run_id: str,
    harness_version: str,
    image_digest: str,
    observed: Mapping[str, ObservedObject],
    launch: Callable[[CheckpointManifest], object],
) -> CheckpointManifest:
    """Bootstrap gate: verify a selected checkpoint end to end, then launch it exactly once.

    Every refusal raises before `launch` is called, so no agent or repository code runs on
    unverified restored state.
    """
    manifest = validate_for_restore(
        data,
        run_id=run_id,
        harness_version=harness_version,
        image_digest=image_digest,
    )
    for ref in (manifest.harness_state, manifest.workspace_state):
        verify_component(ref, observed.get(ref.object_name))
    launch(manifest)
    return manifest
