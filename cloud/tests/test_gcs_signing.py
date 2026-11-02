"""The signer's URL signer over google-cloud-storage (DPES-899 AC2).

Fakes only: no bucket, no IAM call, no credential. These tests pin the V4 signing arguments the
signer depends on (the identity to sign as, the token that authorises signBlob, and the headers
the URL binds), so a change to how URLs are signed fails here rather than in a deployed signer.
"""

from datetime import UTC, datetime
from typing import Any

import pytest

from mao_cloud import gcs_sdk

SIGNER = "mcao-cloud-signer@mao-test.iam.gserviceaccount.com"
EXPIRES = datetime(2026, 10, 23, 12, 1, tzinfo=UTC)
OBJECT = "runs/run_0123456789abcdef0123456789abcdef/attempts/000001/status.json"
CLOUD_PLATFORM = "https://www.googleapis.com/auth/cloud-platform"


class FakeBlob:
    def __init__(self, name: str, calls: list[dict[str, Any]]) -> None:
        self._name = name
        self._calls = calls

    def generate_signed_url(self, **kwargs: Any) -> str:
        self._calls.append({"name": self._name, **kwargs})
        return f"https://storage.test/{self._name}?signed"


class FakeBucket:
    def __init__(self) -> None:
        self.calls: list[dict[str, Any]] = []
        self.names: list[str] = []

    def blob(self, name: str) -> FakeBlob:
        self.names.append(name)
        return FakeBlob(name, self.calls)


class FakeCredentials:
    """Application-default credentials that count refreshes. A refresh issues the next token."""

    def __init__(self) -> None:
        self.valid = False
        self.token: str | None = None
        self.refreshes = 0

    def refresh(self, request: object) -> None:
        self.refreshes += 1
        self.token = f"token-{self.refreshes}"
        self.valid = True


def test_a_url_is_signed_v4_as_the_named_identity_with_the_injected_token() -> None:
    bucket = FakeBucket()
    signer = gcs_sdk._SdkSigner(
        bucket, signing_identity=SIGNER, access_token=lambda: "tok-1"
    )

    url = signer.sign(
        OBJECT, method="PUT", expires_at=EXPIRES, headers={"x-goog-meta-a": "1"}
    )

    assert url == f"https://storage.test/{OBJECT}?signed"
    assert bucket.names == [OBJECT]
    assert bucket.calls == [
        {
            "name": OBJECT,
            "version": "v4",
            "expiration": EXPIRES,
            "method": "PUT",
            "headers": {"x-goog-meta-a": "1"},
            "service_account_email": SIGNER,
            "access_token": "tok-1",
        }
    ]


def test_the_signed_headers_are_a_copy_of_the_caller_mapping() -> None:
    bucket = FakeBucket()
    signer = gcs_sdk._SdkSigner(
        bucket, signing_identity=SIGNER, access_token=lambda: "tok"
    )
    headers = {"x-goog-if-generation-match": "0"}

    signer.sign(OBJECT, method="PUT", expires_at=EXPIRES, headers=headers)

    [call] = bucket.calls
    assert call["headers"] == headers
    assert call["headers"] is not headers


def test_each_signature_takes_the_token_current_at_the_time() -> None:
    bucket = FakeBucket()
    tokens = iter(["tok-1", "tok-2"])
    signer = gcs_sdk._SdkSigner(
        bucket, signing_identity=SIGNER, access_token=lambda: next(tokens)
    )

    signer.sign(OBJECT, method="GET", expires_at=EXPIRES, headers={})
    signer.sign(OBJECT, method="GET", expires_at=EXPIRES, headers={})

    assert [call["access_token"] for call in bucket.calls] == ["tok-1", "tok-2"]


def test_the_token_is_refreshed_only_when_it_has_lapsed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    credentials = FakeCredentials()
    defaults: list[dict[str, Any]] = []

    def fake_default(**kwargs: Any) -> tuple[FakeCredentials, str]:
        defaults.append(kwargs)
        return credentials, "mao-test"

    monkeypatch.setattr(gcs_sdk.google.auth, "default", fake_default)
    source = gcs_sdk._TokenSource()

    assert source() == "token-1"
    assert source() == "token-1"
    assert defaults == [{"scopes": [CLOUD_PLATFORM]}]
    assert credentials.refreshes == 1

    credentials.valid = False
    assert source() == "token-2"
    assert credentials.refreshes == 2
    assert len(defaults) == 1


def test_a_failed_credential_lookup_is_not_swallowed_so_nothing_is_signed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def no_credentials(**_: Any) -> None:
        raise OSError("no application default credentials")

    monkeypatch.setattr(gcs_sdk.google.auth, "default", no_credentials)
    bucket = FakeBucket()
    signer = gcs_sdk._SdkSigner(
        bucket, signing_identity=SIGNER, access_token=gcs_sdk._TokenSource()
    )

    with pytest.raises(OSError):
        signer.sign(OBJECT, method="GET", expires_at=EXPIRES, headers={})

    assert bucket.calls == []


def test_open_signer_names_the_project_bucket_and_signing_identity_it_was_given(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bucket = FakeBucket()
    clients: list[dict[str, Any]] = []

    class FakeClient:
        def __init__(self, *, project: str) -> None:
            clients.append({"project": project})

        def bucket(self, name: str) -> FakeBucket:
            clients[-1]["bucket"] = name
            return bucket

    credentials = FakeCredentials()
    credentials.refresh(None)
    monkeypatch.setattr(gcs_sdk.storage, "Client", FakeClient)
    monkeypatch.setattr(
        gcs_sdk.google.auth, "default", lambda **_: (credentials, "mao-test")
    )

    signer = gcs_sdk.open_signer(
        project="mao-test", name="mao-test-objects", signing_identity=SIGNER
    )
    signer.sign(OBJECT, method="GET", expires_at=EXPIRES, headers={})

    assert clients == [{"project": "mao-test", "bucket": "mao-test-objects"}]
    assert bucket.names == [OBJECT]
    assert bucket.calls[0]["service_account_email"] == SIGNER
    assert bucket.calls[0]["access_token"] == "token-1"
