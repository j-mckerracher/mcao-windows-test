"""The Firestore query and field contract behind the CAF index and retention handoff (DPES-809).

docs/firestore-indexes-and-retention.md states the same contract in prose. These tests fail when the
store starts issuing a query or scan that the document does not list, or when a module starts to
mention the expiry field, which needs the retention decision before it can be written.
"""

import uuid
from collections.abc import Callable
from datetime import date, timedelta
from pathlib import Path
from typing import Any

from fakes import EPOCH, OWNER_A, run_record
from firestore_fake import FakeFirestore
from mao_cloud import firestore_store
from mao_cloud.firestore_store import (
    ADMISSION_SHARDS,
    FirestoreRunStore,
    provision_admission_layout,
)
from mao_cloud.protocol import IdempotencyRecord

# Assembled from parts so this file never spells the forbidden field name out.
EXPIRY_FIELD = "expire" + "_at"

Call = tuple[Any, ...]


class _RecordingTransaction:
    """Delegates every port call and records each query and scan by collection and filter."""

    def __init__(self, inner: Any, calls: list[Call]) -> None:
        self._inner = inner
        self._calls = calls

    def get(self, collection: str, doc_id: str) -> Any:
        return self._inner.get(collection, doc_id)

    def query(self, collection: str, field: str, value: Any) -> Any:
        self._calls.append(("query", collection, field, value))
        return self._inner.query(collection, field, value)

    def scan(self, collection: str) -> Any:
        self._calls.append(("scan", collection))
        return self._inner.scan(collection)

    def create(self, collection: str, doc_id: str, data: Any) -> None:
        self._inner.create(collection, doc_id, data)

    def set(self, collection: str, doc_id: str, data: Any) -> None:
        self._inner.set(collection, doc_id, data)


class _RecordingClient:
    def __init__(self, inner: FakeFirestore) -> None:
        self._inner = inner
        self.calls: list[Call] = []

    def run_transaction(self, work: Callable[[Any], Any]) -> Any:
        return self._inner.run_transaction(
            lambda txn: work(_RecordingTransaction(txn, self.calls))
        )


def _store() -> tuple[FirestoreRunStore, _RecordingClient]:
    fake = FakeFirestore()
    provision_admission_layout(fake, shards=ADMISSION_SHARDS)
    client = _RecordingClient(fake)
    return FirestoreRunStore(client, shards=ADMISSION_SHARDS), client


def _create_queued(store: FirestoreRunStore, number: int) -> Any:
    run = run_record(
        run_id=f"run_{number:032x}",
        owner_id=OWNER_A,
        request_uuid=uuid.UUID(int=number),
    )
    store.create_run(
        run,
        IdempotencyRecord(
            owner_id=run.owner_id,
            request_uuid=run.request_uuid,
            digest=run.manifest_digest,
            run_id=run.run_id,
            created_at=EPOCH,
            expires_at=EPOCH + timedelta(days=1),
        ),
    )
    return run


def test_admission_snapshot_issues_only_equality_queries_and_one_owner_scan() -> None:
    store, client = _store()
    run = _create_queued(store, 1)
    client.calls.clear()

    snapshot = store.admission_snapshot()

    assert [record.run_id for record in snapshot.records] == [run.run_id]
    queries = [call for call in client.calls if call[0] == "query"]
    assert {(call[1], call[2]) for call in queries} == {
        ("runs", "holds"),
        ("runs", "state"),
        ("runs", "attribution_day"),
    }
    assert {(call[2], call[3]) for call in queries if call[2] != "attribution_day"} == {
        ("holds", True),
        ("state", "queued"),
    }
    days = [call[3] for call in queries if call[2] == "attribution_day"]
    assert len(days) == 1
    assert date.fromisoformat(days[0])
    assert [call for call in client.calls if call[0] == "scan"] == [("scan", "owners")]


def test_list_runs_is_one_unfiltered_scan_of_the_runs_collection() -> None:
    store, client = _store()
    run = _create_queued(store, 1)
    client.calls.clear()

    assert [record.run_id for record in store.list_runs()] == [run.run_id]
    assert client.calls == [("scan", "runs")]


def test_no_module_in_the_controller_mentions_the_expiry_field() -> None:
    """A textual proxy for "no writer". Adding the field also needs the retention approval first,
    so the change must update this test and docs/firestore-indexes-and-retention.md together.
    """
    package = Path(firestore_store.__file__).parent
    for source in sorted(package.glob("*.py")):
        assert EXPIRY_FIELD not in source.read_text(encoding="utf-8"), source.name
