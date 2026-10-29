"""Storage port for DPES-869 (decisions Q1 and Q2). No I/O in this module.

Firestore holds authoritative run, pointer and commit-marker records, written only by the
controller. GCS holds immutable payload and checkpoint objects, each created with a
generation precondition. Transcript entries, Documents and tasks are durable records that
commit together or not at all.

Every adapter must meet the contracts stated on these methods. The in-memory fake in
cloud/tests/storage_fakes.py models them. The upstream conformance suite needs an approved real
backend; DPES-807 (cloud state and storage provisioning, a listed DPES-869 prerequisite) supplies
it. Until then the conformance suite is a stated gap.
"""

from collections.abc import Mapping
from contextlib import AbstractContextManager
from dataclasses import dataclass
from enum import StrEnum
from typing import Protocol

from mao_cloud.checkpoint import CommitMarker
from mao_cloud.protocol import ObservedInput, RunRecord


class StorageFailure(Exception):
    """A write did not commit. Nothing from the failed unit is visible to any reader."""


class StorageConflict(StorageFailure):
    """A create-only name or key already exists. Nothing from the refused unit is visible."""


class DurableKind(StrEnum):
    TRANSCRIPT_ENTRY = "transcript_entry"
    DOCUMENT = "document"
    TASK = "task"


@dataclass(frozen=True)
class DurableWrite:
    kind: DurableKind
    key: str
    body: bytes


@dataclass(frozen=True)
class DurableBatch:
    """Writes that apply together or not at all. Each (kind, key) may appear once in a batch."""

    run_id: str
    writes: tuple[DurableWrite, ...]


class Transaction(Protocol):
    """Staged writes. They apply together when the transaction block exits without error."""

    def get_run(self, run_id: str) -> RunRecord | None: ...

    def put_run(self, run: RunRecord, *, expected_version: int) -> None:
        """Replace the stored run only if its version still equals expected_version."""

    def get_marker(self, manifest_name: str) -> CommitMarker | None:
        """Read a marker transactionally; a concurrent change must invalidate this commit."""

    def put_marker(self, marker: CommitMarker) -> None:
        """Create the commit marker for its manifest, applied with the pointer on commit."""

    def put_pointer(self, run_id: str, manifest_name: str) -> None:
        """Point the run's selection at a manifest. The selection moves only on commit."""


class Storage(Protocol):
    def commit(self, batch: DurableBatch) -> None:
        """Apply every write in the batch, or none. Raises StorageConflict for a stored key."""

    def read_durable(
        self, run_id: str, kind: DurableKind, key: str
    ) -> bytes | None: ...

    def create_object(self, name: str, data: bytes) -> int:
        """Create an immutable object only if the name is free. Returns its generation."""

    def head(self, name: str) -> ObservedInput | None: ...

    def read_object(self, name: str) -> bytes | None: ...

    def read_run(self, run_id: str) -> RunRecord | None: ...

    def read_pointer(self, run_id: str) -> str | None:
        """The manifest name the run's selection names, or None before the first commit."""

    def read_marker(self, manifest_name: str) -> CommitMarker | None: ...

    def checkpoint_component_bytes(self, run_id: str) -> int:
        """Bytes of every harness and workspace checkpoint object the run holds, manifests excluded.

        Counts leftovers from failed or uncommitted publishes too, so a retry cannot reset the per-run
        budget. A real adapter must report this from storage itself (DPES-807), never a caller figure.
        """

    def reserve_checkpoint_components(
        self, run_id: str, components: Mapping[str, int], *, max_bytes: int
    ) -> bool:
        """Atomically admit all component names and sizes against the run's stored and pending bytes.

        The storage authority coordinates all processes and handles before any object upload.
        Existing objects and durable reservations count once per name across all attempts;
        manifests do not count. A reservation survives failed or ambiguous writes and is reused
        only for the same name and size. Different sizes raise StorageConflict. Budget refusal
        returns False and reserves nothing. Reservations do not expire or release on failure.
        Every checkpoint publisher must reserve before creating components. Real adapters must
        persist the allocation atomically in the shared backend, never in a process-local lock.
        """

    def transaction(self) -> AbstractContextManager[Transaction]:
        """Stage writes. Apply them on clean exit. Apply nothing if the block raises."""
