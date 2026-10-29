"""google-cloud-firestore behind the narrow port that firestore_store.py defines.

The only module that imports google.cloud.firestore. firestore_store.production_firestore_client()
imports it lazily, so loading the package never loads the SDK. mao_cloud.production composes
open_client() into the controller; that path is unverified against a live database. This module
does no I/O of its own; every call goes through the SDK client that open_client() builds.
"""

from collections.abc import Callable
from typing import Any, TypeVar

from google.api_core import exceptions as api_exceptions
from google.cloud import firestore

from mao_cloud.firestore_store import (
    Document,
    DocumentExists,
    FirestoreTransaction,
    TransactionAborted,
)

T = TypeVar("T")


def _present(snapshot: firestore.DocumentSnapshot) -> tuple[str, Document]:
    data = snapshot.to_dict()
    if data is None:
        raise RuntimeError(f"{snapshot.id} was returned by a query but has no data")
    return snapshot.id, data


class _SdkTransaction:
    """One SDK transaction attempt, seen through the port. Reads must precede writes."""

    def __init__(
        self, transaction: firestore.Transaction, client: firestore.Client
    ) -> None:
        self._transaction = transaction
        self._client = client
        self._staged = False

    def _reading(self) -> None:
        if self._staged:
            raise RuntimeError("read after write in one transaction")

    def get(self, collection: str, doc_id: str) -> Document | None:
        self._reading()
        [snapshot] = list(
            self._transaction.get(self._client.document(collection, doc_id))
        )
        return snapshot.to_dict() if snapshot.exists else None

    def query(
        self, collection: str, field: str, value: Any
    ) -> list[tuple[str, Document]]:
        self._reading()
        matches = self._client.collection(collection).where(
            filter=firestore.FieldFilter(field, "==", value)
        )
        return [_present(s) for s in matches.stream(transaction=self._transaction)]

    def scan(self, collection: str) -> list[tuple[str, Document]]:
        self._reading()
        documents = self._client.collection(collection).stream(
            transaction=self._transaction
        )
        return [_present(s) for s in documents]

    def create(self, collection: str, doc_id: str, data: Document) -> None:
        self._staged = True
        self._transaction.create(self._client.document(collection, doc_id), dict(data))

    def set(self, collection: str, doc_id: str, data: Document) -> None:
        self._staged = True
        self._transaction.set(self._client.document(collection, doc_id), dict(data))


class SdkFirestoreClient:
    """FirestoreClient over one google.cloud.firestore.Client.

    The SDK's transactional decorator does the retry: it reruns the work when commit raises Aborted,
    and rolls back and re-raises anything else unchanged. This class only maps the SDK's exceptions
    onto the port's two.
    """

    def __init__(self, client: firestore.Client) -> None:
        self._client = client

    def run_transaction(self, work: Callable[[FirestoreTransaction], T]) -> T:
        def attempt(transaction: firestore.Transaction) -> T:
            return work(_SdkTransaction(transaction, self._client))

        try:
            return firestore.transactional(attempt)(self._client.transaction())
        except api_exceptions.AlreadyExists as exc:
            raise DocumentExists(str(exc)) from exc
        except api_exceptions.Aborted as exc:
            raise TransactionAborted(str(exc)) from exc
        except ValueError as exc:
            # The SDK raises ValueError, chained to the last Aborted, once its commit retries run out.
            if isinstance(exc.__cause__, api_exceptions.Aborted):
                raise TransactionAborted(str(exc)) from exc
            raise


def open_client(*, project: str, database: str) -> SdkFirestoreClient:
    """Build the SDK client for one named Firestore database. The caller names both; nothing is inferred."""
    return SdkFirestoreClient(firestore.Client(project=project, database=database))
