"""In-memory model of the narrow Firestore port that firestore_store uses. Test-only; not shipped.

It models what the adapter relies on and nothing a real backend would add:
- A transaction records the version of every document it reads. Commit fails with TransactionAborted
  if any of them changed. Work is retried on that abort, up to max_attempts, then the abort is raised.
- create() fails at commit with DocumentExists when the document is present.
- get(), query(), and scan() refuse every read after any write has been staged.
- A query reads the documents it returns but does not protect against documents that start to match
  later (phantom inserts), as Firestore does not.
- Each document's writes are counted per one-second window of the injected clock. With
  strict_window_limit set, a commit that would exceed the limit in its window raises a retryable
  TransactionAborted. This is a conservative test model of the documented soft limit of about one
  sustained write per second per document. It is not the real backend's behaviour.
- Stored values must be plain JSON types, so a datetime or enum in a document is a TypeError.

Nothing here reaches a cloud API. It proves nothing about a real backend.
"""

import copy
from collections.abc import Callable, Mapping
from datetime import datetime
from threading import RLock
from types import NoneType
from typing import Any, TypeVar

from mao_cloud.firestore_store import (
    DocumentExists,
    FirestoreTransaction,
    TransactionAborted,
)

T = TypeVar("T")
_SCALARS = (str, bool, int, float, NoneType)


def _assert_plain(value: Any, where: str) -> None:
    if type(value) in _SCALARS:
        return
    if isinstance(value, Mapping):
        for key, item in value.items():
            if type(key) is not str:
                raise TypeError(f"{where}: key {key!r} is not a string")
            _assert_plain(item, f"{where}.{key}")
        return
    if isinstance(value, list | tuple):
        for index, item in enumerate(value):
            _assert_plain(item, f"{where}[{index}]")
        return
    raise TypeError(f"{where}: {type(value).__name__} is not stored as plain JSON")


class _Txn:
    def __init__(self, fake: "FakeFirestore") -> None:
        self._fake = fake
        self._reads: dict[str, int] = {}
        self._writes: dict[str, tuple[str, dict[str, Any]]] = {}

    def _path(self, collection: str, doc_id: str) -> str:
        return f"{collection}/{doc_id}"

    def _require_read_phase(self, path: str) -> None:
        if self._writes:
            raise AssertionError(f"read after write in one transaction: {path}")

    def _read(self, path: str) -> Any:
        self._require_read_phase(path)
        version = self._fake._versions.get(path, 0)
        self._reads.setdefault(path, version)
        return copy.deepcopy(self._fake._docs.get(path))

    def get(self, collection: str, doc_id: str) -> Mapping[str, Any] | None:
        with self._fake._lock:
            return self._read(self._path(collection, doc_id))

    def query(
        self, collection: str, field: str, value: Any
    ) -> list[tuple[str, Mapping[str, Any]]]:
        with self._fake._lock:
            self._require_read_phase(collection)
            matches: list[tuple[str, Mapping[str, Any]]] = []
            for path, data in self._fake._matching(collection):
                if data.get(field) == value and type(data.get(field)) is type(value):
                    self._reads.setdefault(path, self._fake._versions.get(path, 0))
                    matches.append((path.split("/", 1)[1], copy.deepcopy(data)))
            return matches

    def scan(self, collection: str) -> list[tuple[str, Mapping[str, Any]]]:
        with self._fake._lock:
            self._require_read_phase(collection)
            docs: list[tuple[str, Mapping[str, Any]]] = []
            for path, data in self._fake._matching(collection):
                self._reads.setdefault(path, self._fake._versions.get(path, 0))
                docs.append((path.split("/", 1)[1], copy.deepcopy(data)))
            return docs

    def create(self, collection: str, doc_id: str, data: Mapping[str, Any]) -> None:
        self._stage("create", collection, doc_id, data)

    def set(self, collection: str, doc_id: str, data: Mapping[str, Any]) -> None:
        self._stage("set", collection, doc_id, data)

    def _stage(
        self, kind: str, collection: str, doc_id: str, data: Mapping[str, Any]
    ) -> None:
        path = self._path(collection, doc_id)
        if path in self._writes:
            raise AssertionError(f"second write to {path} in one transaction")
        _assert_plain(data, path)
        self._writes[path] = (kind, copy.deepcopy(dict(data)))

    def commit(self) -> None:
        fake = self._fake
        with fake._lock:
            for path, seen in self._reads.items():
                if fake._versions.get(path, 0) != seen:
                    raise TransactionAborted(f"read document changed: {path}")
            for path, (kind, _) in self._writes.items():
                if kind == "create" and path in fake._docs:
                    raise DocumentExists(path)
            window = fake._window()
            if fake._strict_limit is not None:
                for path in self._writes:
                    if (
                        fake._window_counts.get((path, window), 0) + 1
                        > fake._strict_limit
                    ):
                        raise TransactionAborted(
                            f"per-document write rate exceeded: {path}"
                        )
            for path, (_, data) in self._writes.items():
                fake._docs[path] = data
                fake._versions[path] = fake._versions.get(path, 0) + 1
                count = fake._window_counts.get((path, window), 0) + 1
                fake._window_counts[(path, window)] = count
                fake._peak[path] = max(fake._peak.get(path, 0), count)
                fake._total[path] = fake._total.get(path, 0) + 1


class FakeFirestore:
    """Implements FirestoreClient and exposes what the tests need to observe."""

    def __init__(
        self,
        *,
        clock: Callable[[], datetime] | None = None,
        strict_window_limit: int | None = None,
        max_attempts: int = 5,
    ) -> None:
        self._docs: dict[str, dict[str, Any]] = {}
        self._versions: dict[str, int] = {}
        self._window_counts: dict[tuple[str, int], int] = {}
        self._peak: dict[str, int] = {}
        self._total: dict[str, int] = {}
        self._clock = clock
        self._strict_limit = strict_window_limit
        self._max_attempts = max_attempts
        self._hooks: list[Callable[[], None]] = []
        self._lock = RLock()
        self.attempts = 0
        self.aborts = 0

    # FirestoreClient -------------------------------------------------------

    def run_transaction(self, work: Callable[[FirestoreTransaction], T]) -> T:
        for _ in range(self._max_attempts):
            self.attempts += 1
            txn = _Txn(self)
            try:
                result = work(txn)
                if self._hooks:
                    self._hooks.pop(0)()
                txn.commit()
                return result
            except TransactionAborted:
                self.aborts += 1
        raise TransactionAborted("retries exhausted")

    # Test controls ---------------------------------------------------------

    def interleave(self, action: Callable[[], None]) -> None:
        """Run action just before the next transaction attempt commits. Used to force contention."""
        self._hooks.append(action)

    def document(self, collection: str, doc_id: str) -> dict[str, Any] | None:
        with self._lock:
            data = self._docs.get(f"{collection}/{doc_id}")
            return copy.deepcopy(data)

    def documents(self, collection: str) -> list[dict[str, Any]]:
        with self._lock:
            return [copy.deepcopy(data) for _, data in self._matching(collection)]

    def peak_writes_per_second(self, collection: str, doc_id: str) -> int:
        return self._peak.get(f"{collection}/{doc_id}", 0)

    def total_writes(self, collection: str, doc_id: str) -> int:
        return self._total.get(f"{collection}/{doc_id}", 0)

    # Internals -------------------------------------------------------------

    def _window(self) -> int:
        return int(self._clock().timestamp()) if self._clock is not None else 0

    def _matching(self, collection: str) -> list[tuple[str, dict[str, Any]]]:
        prefix = f"{collection}/"
        return [
            (path, data)
            for path, data in self._docs.items()
            if path.startswith(prefix) and "/" not in path[len(prefix) :]
        ]
