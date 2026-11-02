"""Firestore run store for DPES-809: the RunStore contract over a narrow Firestore port.

No I/O of its own. Every operation is one transaction against FirestoreClient, a port defined here
and implemented by the in-memory fake in cloud/tests/firestore_fake.py and by the SDK adapter in
mao_cloud.firestore_sdk. google-cloud-firestore is not imported at module load.
production_firestore_client() imports the adapter lazily; mao_cloud.production composes it into the
controller's run store from MAO_CLOUD_PROJECT and MAO_CLOUD_FIRESTORE_DATABASE. The admission layout
must be provisioned in the database, or every operation refuses. Nothing here has been run against a
live Firestore database, so live behaviour is unverified.

Document layout, relative to the database root:
  runs/{run_id}            RunRecord under "record", plus the query fields state, holds and
                           attribution_day. Written by create and by every compare-and-set.
  idempotency/{key}        IdempotencyRecord, keyed by (owner_id, request_uuid). A retry's record is
                           stored under its own key and never replaces the create's record.
  requests/{key}           run_id of the create that first claimed (owner_id, request_uuid).
                           Written only by create, as the in-memory store does.
  owners/{key}             The owner's latest dispatch (run_id and its dispatch fields). One document
                           per owner, so admission reads one record per owner, not its history.
  admission_shards/{nnn}   Shard counters. The shared revision is their sum.
  admission_layout/current {"shards": N}. Provisioned once per backend. Any other N refuses every
                           operation, because a changed N would let a sum repeat an old value.
  meta/restore_floor       {"floor": iso} once a restore has set one. Not written in this module.

Shared revision. Each run mutation (create and compare-and-set) increments exactly one shard, chosen
by a hash of its run_id, in the same transaction as its write. Shards only grow, so their sum only
grows. A compare-and-set with expected_revision reads every shard in its own transaction and refuses
unless the sum equals the revision admission_snapshot returned. A mutation committed after the
snapshot changes the sum, so the refusal also covers phantom inserts, which Firestore queries do not
detect. Plain mutations read only their own shard, so they do not serialise against each other.
Per-document write rate is about the mutation rate divided by the shard count, not the full mutation
rate on one document.

Admission snapshot. Reads the shards, then the records admission can consult: holding capacity
(holds == true), queued (state == queued), every record on the attribution day of a queued run, and
the latest dispatched run per owner. Each query uses one equality filter, so no composite index is
needed. Holding and queued records are bounded by the configured slot and queue limits. The day
reads grow with that day's run volume, and the owner scan grows with the number of owners that have
dispatched. A tighter bound waits on DPES-807 indexes. select_next_dispatch gets the same answer on
this subset as on the full record set: every input it reads is one of the records above.
"""

import hashlib
from collections.abc import Callable, Mapping
from datetime import UTC, datetime
from typing import Any, Protocol, TypeVar
from uuid import UUID

from mao_cloud.admission import attribution_day, holds_capacity
from mao_cloud.broker import RunSnapshot
from mao_cloud.protocol import (
    ErrorCode,
    IdempotencyRecord,
    ProtocolError,
    RunRecord,
    RunState,
)

# Fixed for the life of a backend. Sustained capacity is about one mutation per second per shard.
ADMISSION_SHARDS = 16

RUNS = "runs"
IDEMPOTENCY = "idempotency"
REQUESTS = "requests"
OWNERS = "owners"
SHARDS = "admission_shards"
LAYOUT = "admission_layout"
META = "meta"

_EARLIEST = datetime.min.replace(tzinfo=UTC)

Document = Mapping[str, Any]
T = TypeVar("T")


class DocumentExists(Exception):
    """A create-only write found its document present at commit."""


class TransactionAborted(Exception):
    """Contention: a document this transaction read or wrote changed. Retryable."""


class FirestoreTransaction(Protocol):
    """Read before staging any writes. Staged writes apply together on commit."""

    def get(self, collection: str, doc_id: str) -> Document | None: ...

    def query(
        self, collection: str, field: str, value: Any
    ) -> list[tuple[str, Document]]:
        """Documents whose top-level field equals value. Reads the matches only.

        Documents that start to match after this read are not detected, as in Firestore.
        """

    def scan(self, collection: str) -> list[tuple[str, Document]]: ...

    def create(self, collection: str, doc_id: str, data: Document) -> None:
        """Write only if the document is absent. DocumentExists is raised at commit otherwise."""

    def set(self, collection: str, doc_id: str, data: Document) -> None: ...


class FirestoreClient(Protocol):
    def run_transaction(self, work: Callable[[FirestoreTransaction], T]) -> T:
        """Run work, then commit. Retry work only on TransactionAborted.

        Any other exception from work or commit ends the transaction and propagates unchanged.
        """


class FirestoreStoreError(RuntimeError):
    """The backend does not hold the layout or index this store needs. Refuses loudly."""


def provision_admission_layout(
    client: FirestoreClient, *, shards: int = ADMISSION_SHARDS
) -> None:
    """Record the shard count. Idempotent for the same count; refuses a different one."""
    if shards < 1:
        raise ValueError("shards must be at least 1")

    def work(txn: FirestoreTransaction) -> None:
        current = txn.get(LAYOUT, "current")
        if current is None:
            txn.create(LAYOUT, "current", {"shards": shards})
        elif current.get("shards") != shards:
            raise FirestoreStoreError(
                f"admission layout already provisioned for {current.get('shards')} shards"
            )

    client.run_transaction(work)


def shard_index(run_id: str, shards: int) -> int:
    """Stable across processes, unlike hash()."""
    return int.from_bytes(hashlib.sha256(run_id.encode()).digest()[:8], "big") % shards


def _digest(*parts: str) -> str:
    return hashlib.sha256("\x1f".join(parts).encode()).hexdigest()


def _shard_id(index: int) -> str:
    return f"{index:03d}"


def _dispatch_key(
    order: int, dispatched_at: datetime | None
) -> tuple[int, int, datetime]:
    """Mirrors broker._last_dispatch_by_owner, so the owner document matches what it computes."""
    return (
        int(order > 0),
        order,
        _EARLIEST if order > 0 else (dispatched_at or _EARLIEST),
    )


def _run_doc(run: RunRecord) -> dict[str, Any]:
    record = run.model_dump(mode="json")
    if run.execution_name is None:
        record.pop("execution_name")
    return {
        "record": record,
        "state": run.state.value,
        "holds": holds_capacity(run),
        "attribution_day": attribution_day(run).isoformat(),
    }


def _decode_run(doc: Document) -> RunRecord:
    return RunRecord.model_validate(doc["record"])


def _idempotency_doc(record: IdempotencyRecord) -> dict[str, Any]:
    return record.model_dump(mode="json")


def _decode_idempotency(doc: Document) -> IdempotencyRecord:
    return IdempotencyRecord.model_validate(doc)


def _owner_doc(run: RunRecord) -> dict[str, Any]:
    return {
        "owner_id": run.owner_id,
        "run_id": run.run_id,
        "dispatch_order": run.dispatch_order,
        "dispatched_at": run.dispatched_at.isoformat() if run.dispatched_at else None,
    }


def _owner_key(doc: Document) -> tuple[int, int, datetime]:
    raw = doc["dispatched_at"]
    return _dispatch_key(
        doc["dispatch_order"], datetime.fromisoformat(raw) if raw else None
    )


class FirestoreRunStore:
    """RunStore over FirestoreClient. Every method is one transaction."""

    def __init__(
        self, client: FirestoreClient, *, shards: int = ADMISSION_SHARDS
    ) -> None:
        if shards < 1:
            raise ValueError("shards must be at least 1")
        self._client = client
        self._shards = shards

    # Reads -----------------------------------------------------------------

    def get_run(self, run_id: str) -> RunRecord | None:
        return self._transact(lambda txn: self._load_run(txn, run_id))

    def find_run_by_request(
        self, owner_id: str, request_uuid: UUID
    ) -> RunRecord | None:
        def work(txn: FirestoreTransaction) -> RunRecord | None:
            ref = txn.get(REQUESTS, _digest(owner_id, str(request_uuid)))
            return None if ref is None else self._load_run(txn, ref["run_id"])

        return self._transact(work)

    def get_idempotency(
        self, owner_id: str, request_uuid: UUID
    ) -> IdempotencyRecord | None:
        def work(txn: FirestoreTransaction) -> IdempotencyRecord | None:
            doc = txn.get(IDEMPOTENCY, _digest(owner_id, str(request_uuid)))
            return None if doc is None else _decode_idempotency(doc)

        return self._transact(work)

    def restore_floor(self) -> datetime | None:
        def work(txn: FirestoreTransaction) -> datetime | None:
            doc = txn.get(META, "restore_floor")
            return None if doc is None else datetime.fromisoformat(doc["floor"])

        return self._transact(work)

    def admission_snapshot(self) -> RunSnapshot:
        def work(txn: FirestoreTransaction) -> RunSnapshot:
            revision = sum(self._shard_revisions(txn))
            found: dict[str, RunRecord] = {}

            def take(docs: list[tuple[str, Document]]) -> list[RunRecord]:
                runs = [_decode_run(doc) for _, doc in docs]
                for run in runs:
                    found.setdefault(run.run_id, run)
                return runs

            take(txn.query(RUNS, "holds", True))
            queued = take(txn.query(RUNS, "state", RunState.QUEUED.value))
            for day in sorted({attribution_day(run).isoformat() for run in queued}):
                take(txn.query(RUNS, "attribution_day", day))
            for _, owner in txn.scan(OWNERS):
                run = self._load_run(txn, owner["run_id"])
                if run is None:
                    raise FirestoreStoreError(
                        "owner index names a run that is not stored"
                    )
                found.setdefault(run.run_id, run)
            return RunSnapshot(records=tuple(found.values()), revision=revision)

        return self._transact(work)

    def list_runs(self) -> list[RunRecord]:
        return self._transact(
            lambda txn: [_decode_run(doc) for _, doc in txn.scan(RUNS)]
        )

    # Writes ----------------------------------------------------------------

    def create_run(self, run: RunRecord, idempotency: IdempotencyRecord) -> None:
        key = _digest(run.owner_id, str(run.request_uuid))

        def work(txn: FirestoreTransaction) -> None:
            if (
                txn.get(RUNS, run.run_id) is not None
                or txn.get(IDEMPOTENCY, key) is not None
            ):
                raise ProtocolError(
                    ErrorCode.VERSION_CONFLICT, "run or dedup record already exists"
                )
            revision = self._read_shard(txn, shard_index(run.run_id, self._shards))
            owner_update = self._dispatch_update(txn, run)
            txn.create(RUNS, run.run_id, _run_doc(run))
            txn.create(IDEMPOTENCY, key, _idempotency_doc(idempotency))
            txn.create(REQUESTS, key, {"run_id": run.run_id})
            if owner_update is not None:
                txn.set(OWNERS, _digest(run.owner_id), owner_update)
            self._bump(txn, run.run_id, revision)

        self._transact(work)

    def compare_and_set(
        self,
        run: RunRecord,
        *,
        expected_version: int,
        idempotency: IdempotencyRecord | None = None,
        expected_revision: int | None = None,
    ) -> None:
        def work(txn: FirestoreTransaction) -> None:
            if expected_revision is not None:
                actual = sum(self._shard_revisions(txn))
                if expected_revision != actual:
                    raise ProtocolError(
                        ErrorCode.VERSION_CONFLICT,
                        "admission snapshot changed",
                        expected=expected_revision,
                        actual=actual,
                    )
            current = self._load_run(txn, run.run_id)
            if current is None:
                raise ProtocolError(ErrorCode.NOT_FOUND, "run not found")
            if current.version != expected_version:
                raise ProtocolError(
                    ErrorCode.VERSION_CONFLICT,
                    "stored version differs from the version read",
                    expected=expected_version,
                    actual=current.version,
                )
            revision = self._read_shard(txn, shard_index(run.run_id, self._shards))
            owner_update = self._dispatch_update(txn, run)
            txn.set(RUNS, run.run_id, _run_doc(run))
            if idempotency is not None:
                key = _digest(idempotency.owner_id, str(idempotency.request_uuid))
                txn.set(IDEMPOTENCY, key, _idempotency_doc(idempotency))
            if owner_update is not None:
                txn.set(OWNERS, _digest(run.owner_id), owner_update)
            self._bump(txn, run.run_id, revision)

        self._transact(work)

    # Internals -------------------------------------------------------------

    def _transact(self, work: Callable[[FirestoreTransaction], T]) -> T:
        def checked(txn: FirestoreTransaction) -> T:
            self._require_layout(txn)
            return work(txn)

        try:
            return self._client.run_transaction(checked)
        except (DocumentExists, TransactionAborted) as exc:
            raise ProtocolError(
                ErrorCode.VERSION_CONFLICT,
                "concurrent write; retry the request",
                reason=type(exc).__name__,
            ) from exc

    def _require_layout(self, txn: FirestoreTransaction) -> None:
        layout = txn.get(LAYOUT, "current")
        if layout is None or layout.get("shards") != self._shards:
            raise FirestoreStoreError(
                f"admission layout is not provisioned for {self._shards} shards"
            )

    def _read_shard(self, txn: FirestoreTransaction, index: int) -> int:
        doc = txn.get(SHARDS, _shard_id(index))
        return int(doc["revision"]) if doc is not None else 0

    def _shard_revisions(self, txn: FirestoreTransaction) -> list[int]:
        return [self._read_shard(txn, index) for index in range(self._shards)]

    def _bump(self, txn: FirestoreTransaction, run_id: str, revision: int) -> None:
        index = shard_index(run_id, self._shards)
        txn.set(SHARDS, _shard_id(index), {"revision": revision + 1})

    def _load_run(self, txn: FirestoreTransaction, run_id: str) -> RunRecord | None:
        doc = txn.get(RUNS, run_id)
        return None if doc is None else _decode_run(doc)

    def _dispatch_update(
        self, txn: FirestoreTransaction, run: RunRecord
    ) -> dict[str, Any] | None:
        if run.dispatch_order == 0 and run.dispatched_at is None:
            return None
        key = _digest(run.owner_id)
        current = txn.get(OWNERS, key)
        if current is not None and _owner_key(current) >= _dispatch_key(
            run.dispatch_order, run.dispatched_at
        ):
            return None
        return _owner_doc(run)


def production_firestore_client(*, project: str, database: str) -> FirestoreClient:
    """The SDK-backed client for one named database. Not composed into production_adapters.

    The SDK is imported inside this function, so loading this module never loads it. Building the
    client makes no Firestore call; the first operation does, against the database the caller named.
    """
    from mao_cloud.firestore_sdk import open_client

    return open_client(project=project, database=database)
