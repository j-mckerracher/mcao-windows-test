"""The Cloud Storage object store (DPES-809): ObjectStore semantics over a bucket port, and the SDK
bucket adapter over mock google-cloud-storage objects.

No network, no credentials. The SDK's blob and bucket are mocks; google.api_core exceptions are the
real classes the SDK raises. Live create-only behaviour (x-goog-if-generation-match: 0) is unverified
until a deployed run proves it.
"""

import hashlib
from unittest.mock import MagicMock

import pytest
from google.api_core import exceptions as api_exceptions
from google.cloud import storage
from mao_cloud import gcs_sdk
from mao_cloud.gcs_objects import GcsObjectStore, ObjectExists
from mao_cloud.protocol import ErrorCode, ObservedInput, ProtocolError


class DictBucket:
    """A GcsBucket over a dict. Generations count up from 1; create refuses an existing name."""

    def __init__(self) -> None:
        self.objects: dict[str, tuple[int, bytes]] = {}
        self._next_generation = 1

    def stat(self, name: str) -> ObservedInput | None:
        if name not in self.objects:
            return None
        generation, data = self.objects[name]
        return ObservedInput(
            generation=generation,
            size_bytes=len(data),
            sha256=hashlib.sha256(data).hexdigest(),
        )

    def read(self, name: str) -> bytes | None:
        entry = self.objects.get(name)
        return None if entry is None else entry[1]

    def create(self, name: str, data: bytes) -> None:
        if name in self.objects:
            raise ObjectExists(name)
        self.objects[name] = (self._next_generation, data)
        self._next_generation += 1


NAME = "runs/run-1/input/source.tar"


def test_a_missing_object_has_no_head_and_no_body() -> None:
    store = GcsObjectStore(DictBucket())

    assert store.head(NAME) is None
    assert store.get(NAME) is None


def test_create_only_then_head_and_get_return_the_written_object() -> None:
    store = GcsObjectStore(DictBucket())

    store.create_only(NAME, b"abc")

    assert store.head(NAME) == ObservedInput(
        generation=1, size_bytes=3, sha256=hashlib.sha256(b"abc").hexdigest()
    )
    assert store.get(NAME) == b"abc"


def test_a_second_create_only_is_a_version_conflict_and_keeps_the_first_write() -> None:
    store = GcsObjectStore(DictBucket())
    store.create_only(NAME, b"first")

    with pytest.raises(ProtocolError) as refused:
        store.create_only(NAME, b"second")

    assert refused.value.code is ErrorCode.VERSION_CONFLICT
    assert refused.value.detail == {"reason": "ObjectExists"}
    assert str(refused.value) == "object already exists; retry the request"
    assert store.get(NAME) == b"first"


# The SDK bucket adapter, over mock objects only.


def sdk_bucket(bucket: MagicMock) -> gcs_sdk._SdkBucket:
    return gcs_sdk._SdkBucket(bucket)


def test_stat_of_a_missing_blob_is_none() -> None:
    bucket = MagicMock(name="bucket")
    bucket.get_blob.return_value = None

    assert sdk_bucket(bucket).stat(NAME) is None
    bucket.get_blob.assert_called_once_with(NAME)


def test_stat_reports_generation_size_and_the_uploaded_sha256() -> None:
    bucket = MagicMock(name="bucket")
    bucket.get_blob.return_value = MagicMock(
        generation="7", size=12, metadata={"sha256": "ab" * 32}
    )

    assert sdk_bucket(bucket).stat(NAME) == ObservedInput(
        generation=7, size_bytes=12, sha256="ab" * 32
    )


def test_an_object_without_sha256_metadata_reports_an_empty_digest() -> None:
    bucket = MagicMock(name="bucket")
    bucket.get_blob.return_value = MagicMock(generation=3, size=5, metadata=None)

    observed = sdk_bucket(bucket).stat(NAME)

    assert observed is not None
    assert observed.sha256 == ""


def test_read_returns_the_bytes_of_an_existing_blob() -> None:
    bucket = MagicMock(name="bucket")
    bucket.blob.return_value.download_as_bytes.return_value = b"data"

    assert sdk_bucket(bucket).read(NAME) == b"data"
    bucket.blob.assert_called_once_with(NAME)


def test_read_of_a_missing_blob_is_none() -> None:
    bucket = MagicMock(name="bucket")
    bucket.blob.return_value.download_as_bytes.side_effect = api_exceptions.NotFound(
        "gone"
    )

    assert sdk_bucket(bucket).read(NAME) is None


def test_read_propagates_any_other_storage_error() -> None:
    bucket = MagicMock(name="bucket")
    bucket.blob.return_value.download_as_bytes.side_effect = (
        api_exceptions.ServiceUnavailable("try later")
    )

    with pytest.raises(api_exceptions.ServiceUnavailable):
        sdk_bucket(bucket).read(NAME)


def test_create_sends_the_generation_zero_precondition() -> None:
    bucket = MagicMock(name="bucket")

    sdk_bucket(bucket).create(NAME, b"data")

    bucket.blob.return_value.upload_from_string.assert_called_once_with(
        b"data", if_generation_match=0
    )


def test_create_over_an_existing_object_raises_object_exists() -> None:
    bucket = MagicMock(name="bucket")
    bucket.blob.return_value.upload_from_string.side_effect = (
        api_exceptions.PreconditionFailed("exists")
    )

    with pytest.raises(ObjectExists):
        sdk_bucket(bucket).create(NAME, b"data")


def test_open_bucket_names_the_project_and_bucket_without_inference(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    seen: dict[str, str] = {}

    class FakeClient:
        def __init__(self, *, project: str) -> None:
            seen["project"] = project

        def bucket(self, name: str) -> str:
            seen["bucket"] = name
            return "bucket-object"

    monkeypatch.setattr(storage, "Client", FakeClient)

    adapter = gcs_sdk.open_bucket(project="mao-dev", name="mao-dev-objects")

    assert seen == {"project": "mao-dev", "bucket": "mao-dev-objects"}
    assert adapter._bucket == "bucket-object"
