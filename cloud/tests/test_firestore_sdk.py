"""The google-cloud-firestore adapter, with mocks of SDK objects only.

No network, no credentials, no emulator, no live calls. The SDK's gRPC stub and client are mocks.
The SDK's own Transaction, transactional retry, and reference code run unmodified on top of them, so
the commit, retry, and rollback behaviour tested here is the SDK's, not a copy of it. Nothing here is
evidence about a real Firestore backend (DPES-807).
"""

from typing import Any
from unittest.mock import MagicMock

import pytest
from google.api_core import exceptions as api_exceptions
from google.cloud import firestore
from google.cloud.firestore_v1.transaction import MAX_ATTEMPTS
from google.cloud.firestore_v1.types import firestore as firestore_types
from google.cloud.firestore_v1.types import write as write_types

from mao_cloud import firestore_sdk
from mao_cloud.firestore_store import (
    DocumentExists,
    TransactionAborted,
    production_firestore_client,
)

DATABASE = "projects/test-project/databases/(default)"


@pytest.fixture
def grpc() -> MagicMock:
    """The SDK's gRPC stub: the network boundary. No channel is opened."""
    stub = MagicMock(name="firestore_api")
    stub.begin_transaction.side_effect = lambda **_: MagicMock(transaction=b"tx")
    stub.commit.return_value = firestore_types.CommitResponse(
        write_results=[], commit_time=None
    )
    return stub


@pytest.fixture
def sdk(grpc: MagicMock) -> MagicMock:
    """A mock SDK client whose references and transactions are the SDK's real classes."""
    client = MagicMock(spec=firestore.Client, name="Client")
    client._database_string = DATABASE
    client._rpc_metadata = ()
    client._firestore_api = grpc
    client.document.side_effect = lambda *parts: firestore.DocumentReference(
        *parts, client=client
    )
    client.collection.side_effect = lambda *parts: firestore.CollectionReference(
        *parts, client=client
    )
    client.transaction.side_effect = lambda *_, **__: firestore.Transaction(client)
    return client


def snapshot(
    doc_id: str = "doc", data: dict[str, Any] | None = None, *, exists: bool = True
) -> MagicMock:
    shot = MagicMock(spec=firestore.DocumentSnapshot, name=f"snapshot:{doc_id}")
    shot.id = doc_id
    shot.exists = exists
    shot.to_dict.return_value = data if exists else None
    return shot


def adapter(sdk: MagicMock) -> firestore_sdk.SdkFirestoreClient:
    return firestore_sdk.SdkFirestoreClient(sdk)


# Commit, retry, and exception mapping: the SDK's real transactional machinery over the stub.


def test_aborted_commit_reruns_the_work_and_then_commits(
    sdk: MagicMock, grpc: MagicMock
) -> None:
    grpc.commit.side_effect = [
        api_exceptions.Aborted("contention"),
        firestore_types.CommitResponse(write_results=[], commit_time=None),
    ]
    attempts: list[int] = []

    def work(txn: Any) -> str:
        attempts.append(1)
        txn.create("runs", "run-1", {"state": "queued"})
        return "committed"

    assert adapter(sdk).run_transaction(work) == "committed"
    assert len(attempts) == 2
    assert grpc.commit.call_count == 2
    assert grpc.rollback.call_count == 0


def test_aborted_on_every_attempt_becomes_transaction_aborted(
    sdk: MagicMock, grpc: MagicMock
) -> None:
    grpc.commit.side_effect = api_exceptions.Aborted("contention")

    with pytest.raises(TransactionAborted):
        adapter(sdk).run_transaction(lambda txn: txn.create("runs", "run-1", {}))

    assert grpc.commit.call_count == MAX_ATTEMPTS


def test_already_exists_at_commit_becomes_document_exists_without_retry(
    sdk: MagicMock, grpc: MagicMock
) -> None:
    grpc.commit.side_effect = api_exceptions.AlreadyExists("run-1 exists")

    with pytest.raises(DocumentExists):
        adapter(sdk).run_transaction(lambda txn: txn.create("runs", "run-1", {}))

    assert grpc.commit.call_count == 1


def test_other_commit_errors_propagate_unchanged_and_are_not_retried(
    sdk: MagicMock, grpc: MagicMock
) -> None:
    denied = api_exceptions.PermissionDenied("denied")
    grpc.commit.side_effect = denied

    with pytest.raises(api_exceptions.PermissionDenied) as raised:
        adapter(sdk).run_transaction(lambda txn: txn.create("runs", "run-1", {}))

    assert raised.value is denied
    assert grpc.commit.call_count == 1
    assert grpc.rollback.call_count == 1


def test_work_error_propagates_unchanged_without_retry_or_commit(
    sdk: MagicMock, grpc: MagicMock
) -> None:
    failure = KeyError("work failed")
    attempts: list[int] = []

    def work(txn: Any) -> None:
        attempts.append(1)
        raise failure

    with pytest.raises(KeyError) as raised:
        adapter(sdk).run_transaction(work)

    assert raised.value is failure
    assert len(attempts) == 1
    grpc.commit.assert_not_called()
    assert grpc.rollback.call_count == 1


def test_aborted_during_a_read_becomes_transaction_aborted_once(
    sdk: MagicMock,
) -> None:
    sdk.get_all.side_effect = api_exceptions.Aborted("read contention")
    attempts: list[int] = []

    def work(txn: Any) -> None:
        attempts.append(1)
        txn.get("runs", "run-1")

    with pytest.raises(TransactionAborted):
        adapter(sdk).run_transaction(work)

    assert len(attempts) == 1


# Reads and writes through the port.


def test_get_returns_the_document_data_or_none(sdk: MagicMock) -> None:
    sdk.get_all.return_value = [snapshot(exists=False)]
    assert adapter(sdk).run_transaction(lambda txn: txn.get("runs", "missing")) is None

    sdk.get_all.return_value = [snapshot("run-1", {"state": "queued"})]
    assert adapter(sdk).run_transaction(lambda txn: txn.get("runs", "run-1")) == {
        "state": "queued"
    }
    (refs,) = sdk.get_all.call_args.args
    assert [(ref.parent.id, ref.id) for ref in refs] == [("runs", "run-1")]


def test_query_matches_one_equality_filter_and_reads_inside_the_transaction(
    sdk: MagicMock,
) -> None:
    collection = MagicMock(spec=firestore.CollectionReference, name="runs")
    matches = MagicMock(spec=firestore.Query, name="matches")
    matches.stream.return_value = [snapshot("run-1", {"state": "queued"})]
    collection.where.return_value = matches
    sdk.collection.side_effect = None
    sdk.collection.return_value = collection

    result = adapter(sdk).run_transaction(
        lambda txn: txn.query("runs", "state", "queued")
    )

    assert result == [("run-1", {"state": "queued"})]
    fltr = collection.where.call_args.kwargs["filter"]
    assert isinstance(fltr, firestore.FieldFilter)
    assert (fltr.field_path, fltr.value) == ("state", "queued")
    assert fltr.op_string == "=="
    assert isinstance(
        matches.stream.call_args.kwargs["transaction"], firestore.Transaction
    )


def test_scan_reads_the_whole_collection_inside_the_transaction(
    sdk: MagicMock,
) -> None:
    collection = MagicMock(spec=firestore.CollectionReference, name="owners")
    collection.stream.return_value = [snapshot("owner-a", {"run_id": "run-1"})]
    sdk.collection.side_effect = None
    sdk.collection.return_value = collection

    result = adapter(sdk).run_transaction(lambda txn: txn.scan("owners"))

    assert result == [("owner-a", {"run_id": "run-1"})]
    assert isinstance(
        collection.stream.call_args.kwargs["transaction"], firestore.Transaction
    )


def test_a_query_match_without_data_is_refused(sdk: MagicMock) -> None:
    collection = MagicMock(spec=firestore.CollectionReference, name="runs")
    matches = MagicMock(spec=firestore.Query, name="matches")
    matches.stream.return_value = [snapshot("gone", exists=False)]
    collection.where.return_value = matches
    sdk.collection.side_effect = None
    sdk.collection.return_value = collection

    with pytest.raises(RuntimeError, match="gone was returned by a query"):
        adapter(sdk).run_transaction(lambda txn: txn.query("runs", "state", "queued"))


@pytest.mark.parametrize(
    "read",
    [
        ("get", ("runs", "run-1")),
        ("query", ("runs", "state", "queued")),
        ("scan", ("runs",)),
    ],
)
def test_a_read_after_a_staged_write_is_refused_before_commit(
    sdk: MagicMock, grpc: MagicMock, read: tuple[str, tuple[Any, ...]]
) -> None:
    name, args = read

    def work(txn: Any) -> None:
        txn.create("runs", "run-1", {"state": "queued"})
        getattr(txn, name)(*args)

    with pytest.raises(RuntimeError, match="read after write in one transaction"):
        adapter(sdk).run_transaction(work)

    grpc.commit.assert_not_called()


def test_create_is_a_create_only_precondition_and_set_is_not(
    sdk: MagicMock, grpc: MagicMock
) -> None:
    def work(txn: Any) -> None:
        txn.create("runs", "run-1", {"state": "queued"})
        txn.set("admission_shards", "001", {"revision": 1})

    adapter(sdk).run_transaction(work)

    created, replaced = grpc.commit.call_args.kwargs["request"]["writes"]
    assert created.update.name == f"{DATABASE}/documents/runs/run-1"
    assert write_types.Write.pb(created).HasField("current_document")
    assert created.current_document.exists is False
    assert replaced.update.name == f"{DATABASE}/documents/admission_shards/001"
    assert not write_types.Write.pb(replaced).HasField("current_document")


# Construction. The SDK client is mocked; no credentials are resolved.


def test_production_client_names_the_database_and_is_not_built_at_load(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    constructor = MagicMock(spec=firestore.Client, name="Client")
    monkeypatch.setattr(firestore, "Client", constructor)

    client = production_firestore_client(project="test-project", database="mao-cloud")

    constructor.assert_called_once_with(project="test-project", database="mao-cloud")
    assert isinstance(client, firestore_sdk.SdkFirestoreClient)


def test_production_client_requires_the_project_and_database_to_be_named() -> None:
    with pytest.raises(TypeError):
        production_firestore_client()  # type: ignore[call-arg]
