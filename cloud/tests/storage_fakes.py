"""In-memory Storage fake for DPES-869: all-or-none commits, create-only objects, CAS on runs.

Durable state lives in one _Backing. Only a commit or a clean transaction exit writes to it, in a
single step after every check has passed, so a reader sees a whole batch or none of it. reopen()
hands a second handle to the same backing, which stands in for a process restart.
"""

from collections.abc import Callable, Iterator, Mapping
from contextlib import contextmanager
from threading import RLock
from typing import Any

from mao_cloud.checkpoint import CommitMarker
from mao_cloud.protocol import (
    ErrorCode,
    ObservedInput,
    ProtocolError,
    RunRecord,
    sha256_hex,
)
from mao_cloud.storage import (
    DurableBatch,
    DurableKind,
    StorageConflict,
    StorageFailure,
    Transaction,
)

DurableKey = tuple[str, DurableKind, str]


class _Backing:
    def __init__(self) -> None:
        self.lock = RLock()
        self.runs: dict[str, RunRecord] = {}
        self.durable: dict[DurableKey, bytes] = {}
        self.objects: dict[str, tuple[int, bytes]] = {}
        self.pointers: dict[str, str] = {}
        self.markers: dict[str, CommitMarker] = {}
        self.generations = 0
        self.checkpoint_reservations: dict[tuple[str, str], int] = {}


class _StagedTransaction:
    def __init__(self, backing: _Backing) -> None:
        self._backing = backing
        self._runs: dict[str, RunRecord] = {}
        self._markers: dict[str, CommitMarker] = {}
        self._read_markers: dict[str, CommitMarker | None] = {}
        self._pointers: dict[str, str] = {}

    def get_run(self, run_id: str) -> RunRecord | None:
        if run_id in self._runs:
            return self._runs[run_id]
        return self._backing.runs.get(run_id)

    def put_run(self, run: RunRecord, *, expected_version: int) -> None:
        stored = self._backing.runs.get(run.run_id)
        if stored is None:
            raise ProtocolError(ErrorCode.NOT_FOUND, "run is not stored")
        if stored.version != expected_version:
            raise ProtocolError(
                ErrorCode.VERSION_CONFLICT,
                "stored run version moved",
                expected_version=expected_version,
                current_version=stored.version,
            )
        self._runs[run.run_id] = run

    def get_marker(self, manifest_name: str) -> CommitMarker | None:
        if manifest_name in self._markers:
            return self._markers[manifest_name]
        if manifest_name not in self._read_markers:
            self._read_markers[manifest_name] = self._backing.markers.get(manifest_name)
        return self._read_markers[manifest_name]

    def put_marker(self, marker: CommitMarker) -> None:
        self._markers[marker.manifest.object_name] = marker

    def put_pointer(self, run_id: str, manifest_name: str) -> None:
        self._pointers[run_id] = manifest_name

    def apply(self) -> None:
        with self._backing.lock:
            for name, observed in self._read_markers.items():
                if self._backing.markers.get(name) != observed:
                    raise StorageConflict("a checkpoint marker changed during the transaction")
            self._backing.runs.update(self._runs)
            self._backing.markers.update(self._markers)
            self._backing.pointers.update(self._pointers)


class InMemoryStorage:
    def __init__(self, backing: _Backing | None = None) -> None:
        self._backing = backing if backing is not None else _Backing()
        self._fail_commit_at: int | None = None

    def seed_run(self, run: RunRecord) -> None:
        self._backing.runs[run.run_id] = run

    def reopen(self) -> "InMemoryStorage":
        return InMemoryStorage(self._backing)

    def fail_commit_at(self, index: int) -> None:
        """Fail the next commit when it reaches write `index`. The fault is consumed once."""
        self._fail_commit_at = index

    def object_names(self) -> list[str]:
        return sorted(self._backing.objects)

    def corrupt_object(self, name: str, data: bytes) -> None:
        """Replace stored bytes under the same generation, as damage at rest would."""
        generation, _ = self._backing.objects[name]
        self._backing.objects[name] = (generation, data)

    def remove_object(self, name: str) -> None:
        """Drop a stored object, as an out-of-band loss would."""
        del self._backing.objects[name]

    def lifecycle_metadata(self) -> dict[str, Any]:
        """Run, pointer and marker records only. Object bodies are never part of this view."""
        return {
            "runs": {
                run_id: run.model_dump(mode="json")
                for run_id, run in self._backing.runs.items()
            },
            "pointers": dict(self._backing.pointers),
            "markers": {
                name: marker.model_dump(mode="json")
                for name, marker in self._backing.markers.items()
            },
        }

    def commit(self, batch: DurableBatch) -> None:
        keys = [(batch.run_id, write.kind, write.key) for write in batch.writes]
        if len(set(keys)) != len(keys):
            raise StorageConflict("batch repeats a key")
        if any(key in self._backing.durable for key in keys):
            raise StorageConflict("a key in the batch is already stored")
        staged: dict[DurableKey, bytes] = {}
        for index, write in enumerate(batch.writes):
            if self._fail_commit_at == index:
                self._fail_commit_at = None
                raise StorageFailure("injected commit failure")
            staged[(batch.run_id, write.kind, write.key)] = write.body
        self._backing.durable.update(staged)

    def read_durable(self, run_id: str, kind: DurableKind, key: str) -> bytes | None:
        return self._backing.durable.get((run_id, kind, key))

    def create_object(self, name: str, data: bytes) -> int:
        with self._backing.lock:
            if name in self._backing.objects:
                raise StorageConflict("create-only object already exists")
            self._backing.generations += 1
            self._backing.objects[name] = (self._backing.generations, data)
            return self._backing.generations

    def head(self, name: str) -> ObservedInput | None:
        stored = self._backing.objects.get(name)
        if stored is None:
            return None
        generation, data = stored
        return ObservedInput(
            generation=generation, size_bytes=len(data), sha256=sha256_hex(data)
        )

    def read_object(self, name: str) -> bytes | None:
        stored = self._backing.objects.get(name)
        return None if stored is None else stored[1]

    def read_run(self, run_id: str) -> RunRecord | None:
        return self._backing.runs.get(run_id)

    def read_pointer(self, run_id: str) -> str | None:
        return self._backing.pointers.get(run_id)

    def read_marker(self, manifest_name: str) -> CommitMarker | None:
        return self._backing.markers.get(manifest_name)

    def checkpoint_component_bytes(self, run_id: str) -> int:
        prefix = f"runs/{run_id}/"
        return sum(
            len(data)
            for name, (_, data) in self._backing.objects.items()
            if name.startswith(prefix)
            and "/checkpoints/" in name
            and not name.endswith("/manifest.json")
        )

    def reserve_checkpoint_components(
        self, run_id: str, components: Mapping[str, int], *, max_bytes: int
    ) -> bool:
        with self._backing.lock:
            prefix = f"runs/{run_id}/"
            allocations = {
                name: size
                for (owner, name), size in self._backing.checkpoint_reservations.items()
                if owner == run_id
            }
            for name, (_, data) in self._backing.objects.items():
                if name.startswith(prefix) and "/checkpoints/" in name and not name.endswith("/manifest.json"):
                    if name in allocations and allocations[name] != len(data):
                        raise StorageConflict("stored checkpoint size differs from its reservation")
                    allocations[name] = len(data)
            for name, size in components.items():
                if name in allocations and allocations[name] != size:
                    raise StorageConflict("checkpoint name is reserved with a different size")
                allocations[name] = size
            if sum(allocations.values()) > max_bytes:
                return False
            self._backing.checkpoint_reservations.update(
                {(run_id, name): size for name, size in components.items()}
            )
            return True

    @contextmanager
    def transaction(self) -> Iterator[Transaction]:
        staged = _StagedTransaction(self._backing)
        yield staged
        staged.apply()


class FaultingStorage:
    """Delegates to an InMemoryStorage and injects faults at the publication boundaries.

    after_create: raise StorageFailure after the first created object whose name ends with this suffix.
    after_transaction_body: raise StorageFailure after the transaction body, before it applies.
    on_create: runs after every create, for interleaving a concurrent writer.
    unreadable: head() reports no object for any name ending with this suffix (read-back failure).
    events: "create:<name>" and "transaction", in call order.
    """

    def __init__(
        self,
        inner: InMemoryStorage,
        *,
        after_create: str | None = None,
        after_transaction_body: bool = False,
        on_create: Callable[[str], None] | None = None,
        unreadable: str | None = None,
    ) -> None:
        self._inner = inner
        self._after_create = after_create
        self._after_transaction_body = after_transaction_body
        self._on_create = on_create
        self._unreadable = unreadable
        self._create_faulted = False
        self.events: list[str] = []

    def commit(self, batch: DurableBatch) -> None:
        self._inner.commit(batch)

    def read_durable(self, run_id: str, kind: DurableKind, key: str) -> bytes | None:
        return self._inner.read_durable(run_id, kind, key)

    def create_object(self, name: str, data: bytes) -> int:
        self.events.append(f"create:{name}")
        generation = self._inner.create_object(name, data)
        if self._on_create is not None:
            self._on_create(name)
        if (
            self._after_create is not None
            and name.endswith(self._after_create)
            and not self._create_faulted
        ):
            self._create_faulted = True
            raise StorageFailure("injected fault after create")
        return generation

    def head(self, name: str) -> ObservedInput | None:
        if self._unreadable is not None and name.endswith(self._unreadable):
            return None
        return self._inner.head(name)

    def read_object(self, name: str) -> bytes | None:
        return self._inner.read_object(name)

    def read_run(self, run_id: str) -> RunRecord | None:
        return self._inner.read_run(run_id)

    def read_pointer(self, run_id: str) -> str | None:
        return self._inner.read_pointer(run_id)

    def read_marker(self, manifest_name: str) -> CommitMarker | None:
        return self._inner.read_marker(manifest_name)

    def checkpoint_component_bytes(self, run_id: str) -> int:
        return self._inner.checkpoint_component_bytes(run_id)

    def reserve_checkpoint_components(
        self, run_id: str, components: Mapping[str, int], *, max_bytes: int
    ) -> bool:
        return self._inner.reserve_checkpoint_components(run_id, components, max_bytes=max_bytes)

    @contextmanager
    def transaction(self) -> Iterator[Transaction]:
        self.events.append("transaction")
        with self._inner.transaction() as txn:
            yield txn
            if self._after_transaction_body:
                raise StorageFailure("injected fault after transaction body")
