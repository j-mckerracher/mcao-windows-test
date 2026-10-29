"""google-cloud-storage behind the narrow bucket port that gcs_objects.py defines.

The only module that imports google.cloud.storage. production.py imports it lazily, so loading the
package never loads the SDK. The bucket port does no I/O of its own: every call goes through the
bucket that open_bucket() builds, and the SDK's not-found and precondition errors are mapped onto
the port's return value and ObjectExists. open_signer() builds the URL signer the signer service
uses (DPES-899); it signs through IAM, so the signer needs no private key.
"""

from collections.abc import Callable, Mapping
from datetime import datetime
from typing import Any

import google.auth
from google.api_core import exceptions as api_exceptions
from google.auth.transport import requests as google_requests
from google.cloud import storage

from mao_cloud.gcs_objects import ObjectExists
from mao_cloud.protocol import ObservedInput

_CLOUD_PLATFORM = "https://www.googleapis.com/auth/cloud-platform"


class _SdkBucket:
    def __init__(self, bucket: storage.Bucket) -> None:
        self._bucket = bucket

    def stat(self, name: str) -> ObservedInput | None:
        blob = self._bucket.get_blob(name)
        if blob is None:
            return None
        metadata = blob.metadata or {}
        return ObservedInput(
            generation=int(blob.generation),
            size_bytes=int(blob.size or 0),
            sha256=str(metadata.get("sha256", "")),
        )

    def read(self, name: str) -> bytes | None:
        try:
            return self._bucket.blob(name).download_as_bytes()
        except api_exceptions.NotFound:
            return None

    def create(self, name: str, data: bytes) -> None:
        try:
            self._bucket.blob(name).upload_from_string(data, if_generation_match=0)
        except api_exceptions.PreconditionFailed as exc:
            raise ObjectExists(name) from exc


def open_bucket(*, project: str, name: str) -> _SdkBucket:
    """Build the SDK bucket for one named bucket. The caller names both; nothing is inferred.

    Building the client makes no Cloud Storage call; the first operation does.
    """
    return _SdkBucket(storage.Client(project=project).bucket(name))


class _TokenSource:
    """The signer's own access token, refreshed when it lapses. It is used only to call signBlob."""

    def __init__(self) -> None:
        self._credentials: Any = None

    def __call__(self) -> str:
        if self._credentials is None:
            self._credentials, _ = google.auth.default(scopes=[_CLOUD_PLATFORM])
        if not self._credentials.valid:
            self._credentials.refresh(google_requests.Request())
        return str(self._credentials.token)


class _SdkSigner:
    """Signs V4 object URLs for one bucket as one named service account.

    The runtime credentials carry no private key, so each signature is made by IAM signBlob with
    the access token. The signing identity is named by the caller and is never inferred.
    """

    def __init__(
        self,
        bucket: storage.Bucket,
        *,
        signing_identity: str,
        access_token: Callable[[], str],
    ) -> None:
        self._bucket = bucket
        self._identity = signing_identity
        self._access_token = access_token

    def sign(
        self,
        name: str,
        *,
        method: str,
        expires_at: datetime,
        headers: Mapping[str, str],
    ) -> str:
        return self._bucket.blob(name).generate_signed_url(
            version="v4",
            expiration=expires_at,
            method=method,
            headers=dict(headers),
            service_account_email=self._identity,
            access_token=self._access_token(),
        )


def open_signer(*, project: str, name: str, signing_identity: str) -> _SdkSigner:
    """Build the URL signer for one named bucket. Building makes no call; each signature calls IAM."""
    bucket = storage.Client(project=project).bucket(name)
    return _SdkSigner(
        bucket, signing_identity=signing_identity, access_token=_TokenSource()
    )
