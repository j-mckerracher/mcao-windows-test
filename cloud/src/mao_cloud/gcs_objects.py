"""Object store over a narrow bucket port: the ObjectStore contract for DPES-809.

No I/O of its own. The bucket port is implemented by the in-memory fake in cloud/tests/fakes.py and
by the SDK adapter in mao_cloud.gcs_sdk. google-cloud-storage is not imported here.

Every object name is run-prefixed (see protocol.object_name), so one bucket holds every kind. A
create-only write is a precondition on generation zero: the SDK adapter sends it as
x-goog-if-generation-match: 0, and an existing object raises ObjectExists, which this module maps
onto the protocol's VERSION_CONFLICT. Nothing here proves that behaviour against Cloud Storage.
"""

from typing import Protocol

from mao_cloud.protocol import ErrorCode, ObservedInput, ProtocolError


class ObjectExists(Exception):
    """A create-only write found the object already present."""


class GcsBucket(Protocol):
    def stat(self, name: str) -> ObservedInput | None:
        """Generation, size and the uploaded sha256 of the object, or None when absent.

        The sha256 is the object's own sha256 metadata. An object without it reports "", which
        never equals a receipt's digest, so the source check refuses it rather than trusting it.
        """

    def read(self, name: str) -> bytes | None:
        """The object's bytes, or None when absent."""

    def create(self, name: str, data: bytes) -> None:
        """Write name only if it does not exist. Raises ObjectExists if it does."""


class GcsObjectStore:
    """ObjectStore over a GcsBucket. Each method is one bucket call."""

    def __init__(self, bucket: GcsBucket) -> None:
        self._bucket = bucket

    def head(self, name: str) -> ObservedInput | None:
        return self._bucket.stat(name)

    def get(self, name: str) -> bytes | None:
        return self._bucket.read(name)

    def create_only(self, name: str, data: bytes) -> None:
        try:
            self._bucket.create(name, data)
        except ObjectExists as exc:
            raise ProtocolError(
                ErrorCode.VERSION_CONFLICT,
                "object already exists; retry the request",
                reason="ObjectExists",
            ) from exc
