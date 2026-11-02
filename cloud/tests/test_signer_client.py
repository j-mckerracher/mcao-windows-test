"""The controller's production signer client (DPES-899 AC3).

Fakes only: the post and the token source are injected, so no request leaves the test and no
credential is read. These pin what the controller sends (one bearer token for the signer origin, no
redirects, a bounded wait) and how it reads each answer: a capability is returned as the signer named
it, a refusal keeps the signer's code, and anything else is an unavailable signer. Live calls to a
deployed signer are unverified.
"""

import logging
from datetime import UTC, datetime
from typing import Any

import pytest
import requests
from mao_cloud import signer_client
from mao_cloud.protocol import ObjectKind
from mao_cloud.signer import (
    CapabilityRequest,
    IssuedCapability,
    RefusalCode,
    SignerRefused,
)
from mao_cloud.signer_client import TIMEOUT_SECONDS, HttpSignerClient
from mao_cloud.signer_wire import (
    CAPABILITY_PATH,
    refusal_body,
    request_body,
    status_for,
)

ORIGIN = "https://mcao-cloud-signer-test.a.run.app"
TOKEN = "id-token-fake-0001"
EXPIRES_AT = datetime(2030, 1, 1, 0, 5, tzinfo=UTC)
OBJECT = "runs/run_0123456789abcdef0123456789abcdef/attempts/000001/status.json"
ISSUED_BODY: dict[str, Any] = {
    "object_name": OBJECT,
    "method": "PUT",
    "url": "https://storage.test/signed?sig=fake",
    "headers": {"x-goog-if-generation-match": "1"},
    "expires_at": "2030-01-01T00:05:00+00:00",
}
ISSUED = IssuedCapability(
    object_name=OBJECT,
    method="PUT",
    url="https://storage.test/signed?sig=fake",
    headers={"x-goog-if-generation-match": "1"},
    expires_at=EXPIRES_AT,
)


def capability_request() -> CapabilityRequest:
    return CapabilityRequest(
        owner_id="owner-a",
        run_id="run_0123456789abcdef0123456789abcdef",
        kind=ObjectKind.STATUS,
        method="PUT",
        object_name=OBJECT,
        expires_at=EXPIRES_AT,
        attempt=1,
        fence=1,
        claim_id="claim-1",
        expected_generation=1,
    )


class FakeAnswer:
    def __init__(
        self, status_code: int, body: object = None, *, json_error: bool = False
    ) -> None:
        self.status_code = status_code
        self._body = body
        self._json_error = json_error

    def json(self) -> object:
        if self._json_error:
            raise ValueError("not json")
        return self._body


class FakePost:
    def __init__(
        self, answer: FakeAnswer | None = None, error: Exception | None = None
    ) -> None:
        self.calls: list[dict[str, Any]] = []
        self._answer = answer
        self._error = error

    def __call__(self, url: str, **kwargs: Any) -> FakeAnswer:
        self.calls.append({"url": url, **kwargs})
        if self._error is not None:
            raise self._error
        assert self._answer is not None
        return self._answer


class FakeToken:
    def __init__(self, error: Exception | None = None) -> None:
        self.audiences: list[str] = []
        self._error = error

    def __call__(self, audience: str) -> str:
        self.audiences.append(audience)
        if self._error is not None:
            raise self._error
        return TOKEN


def client(post: FakePost, token: FakeToken | None = None) -> HttpSignerClient:
    return HttpSignerClient(
        origin=ORIGIN, post=post, fetch_token=FakeToken() if token is None else token
    )


def _without(field: str) -> dict[str, Any]:
    return {key: value for key, value in ISSUED_BODY.items() if key != field}


# A capability


def test_the_signer_answer_is_returned_as_the_signer_named_it() -> None:
    post = FakePost(FakeAnswer(200, ISSUED_BODY))
    request = capability_request()

    issued = client(post).issue(request)

    assert issued == ISSUED
    [call] = post.calls
    assert call["url"] == ORIGIN + CAPABILITY_PATH
    assert call["json"] == request_body(request)
    assert call["headers"] == {
        "Authorization": f"Bearer {TOKEN}",
        "X-Serverless-Authorization": f"Bearer {TOKEN}",
    }
    assert call["timeout"] == TIMEOUT_SECONDS
    assert call["allow_redirects"] is False


def test_the_identity_token_is_fetched_for_the_origin_as_its_audience() -> None:
    post = FakePost(FakeAnswer(200, ISSUED_BODY))
    token = FakeToken()

    client(post, token).issue(capability_request())

    assert token.audiences == [ORIGIN]


def test_the_default_token_source_asks_google_for_an_id_token_for_the_origin(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fetched: list[tuple[object, str]] = []

    def fake_fetch(request: object, audience: str) -> str:
        fetched.append((request, audience))
        return TOKEN

    monkeypatch.setattr(signer_client.id_token, "fetch_id_token", fake_fetch)
    post = FakePost(FakeAnswer(200, ISSUED_BODY))
    client_with_default_token = HttpSignerClient(origin=ORIGIN, post=post)

    client_with_default_token.issue(capability_request())

    [(request, audience)] = fetched
    assert audience == ORIGIN
    assert isinstance(request, signer_client.google_requests.Request)
    assert post.calls[0]["headers"] == {
        "Authorization": f"Bearer {TOKEN}",
        "X-Serverless-Authorization": f"Bearer {TOKEN}",
    }


# A refusal keeps the signer's code and its fixed message


@pytest.mark.parametrize("code", list(RefusalCode))
def test_a_refusal_from_the_signer_keeps_its_code_and_fixed_message(
    code: RefusalCode,
) -> None:
    post = FakePost(FakeAnswer(status_for(code), refusal_body(code)))

    with pytest.raises(SignerRefused) as refused:
        client(post).issue(capability_request())

    assert refused.value.code is code
    assert str(refused.value) == str(SignerRefused(code))


# An unavailable signer leaves the signing outcome unknown


UNNAMED_ANSWERS = [
    pytest.param(
        FakeAnswer(403, {"refusal": "something_new", "message": "x"}), id="unknown-code"
    ),
    pytest.param(FakeAnswer(403, {"message": "no code"}), id="no-code"),
    pytest.param(FakeAnswer(403, ["a", "list"]), id="list-body"),
    pytest.param(FakeAnswer(500, json_error=True), id="not-json"),
    pytest.param(FakeAnswer(500, None), id="empty-body"),
]


@pytest.mark.parametrize("answer", UNNAMED_ANSWERS)
def test_an_answer_that_names_no_known_refusal_is_unavailable(
    answer: FakeAnswer,
) -> None:
    with pytest.raises(SignerRefused) as refused:
        client(FakePost(answer)).issue(capability_request())

    assert refused.value.code is RefusalCode.UNAVAILABLE


WRONG_SHAPES = [
    pytest.param(_without("url"), id="missing-url"),
    pytest.param({**ISSUED_BODY, "extra": "x"}, id="extra-field"),
    pytest.param(
        {**ISSUED_BODY, "expires_at": "2030-01-01T00:05:00"}, id="naive-expiry"
    ),
    pytest.param({**ISSUED_BODY, "expires_at": 1893456300}, id="numeric-expiry"),
    pytest.param({**ISSUED_BODY, "headers": {"x": 1}}, id="non-string-header"),
    pytest.param({**ISSUED_BODY, "object_name": None}, id="null-object-name"),
    pytest.param({"refusal": "blocked", "message": "x"}, id="refusal-body-on-200"),
    pytest.param(["not", "an", "object"], id="list-body"),
]


@pytest.mark.parametrize("body", WRONG_SHAPES)
def test_a_200_answer_of_the_wrong_shape_is_unavailable(body: object) -> None:
    with pytest.raises(SignerRefused) as refused:
        client(FakePost(FakeAnswer(200, body))).issue(capability_request())

    assert refused.value.code is RefusalCode.UNAVAILABLE


def test_a_redirect_is_not_followed_and_is_unavailable() -> None:
    post = FakePost(FakeAnswer(302, None))

    with pytest.raises(SignerRefused) as refused:
        client(post).issue(capability_request())

    assert refused.value.code is RefusalCode.UNAVAILABLE
    assert post.calls[0]["allow_redirects"] is False


TRANSPORT_FAILURES = [
    pytest.param(requests.ConnectionError("connect-detail-7f3a"), id="connection"),
    pytest.param(requests.Timeout("timeout-detail-7f3a"), id="timeout"),
    pytest.param(requests.exceptions.SSLError("tls-detail-7f3a"), id="tls"),
]


@pytest.mark.parametrize("error", TRANSPORT_FAILURES)
def test_a_transport_failure_is_unavailable_and_withholds_its_detail(
    error: Exception,
) -> None:
    with pytest.raises(SignerRefused) as refused:
        client(FakePost(error=error)).issue(capability_request())

    assert refused.value.code is RefusalCode.UNAVAILABLE
    assert "7f3a" not in str(refused.value)


@pytest.mark.parametrize("error", TRANSPORT_FAILURES)
def test_a_failed_call_after_the_request_is_sent_leaves_the_outcome_unknown(
    error: Exception,
) -> None:
    # The signer may have issued the capability before the connection failed.
    with pytest.raises(SignerRefused) as refused:
        client(FakePost(error=error)).issue(capability_request())

    assert "unknown" in str(refused.value)
    assert "nothing" not in str(refused.value)


UNREADABLE_ANSWERS = [
    pytest.param(FakeAnswer(200, json_error=True), id="unreadable-200"),
    pytest.param(FakeAnswer(200, _without("url")), id="wrong-shape-200"),
    pytest.param(FakeAnswer(500, json_error=True), id="unreadable-500"),
]


@pytest.mark.parametrize("answer", UNREADABLE_ANSWERS)
def test_an_unreadable_answer_after_the_request_is_sent_leaves_the_outcome_unknown(
    answer: FakeAnswer,
) -> None:
    with pytest.raises(SignerRefused) as refused:
        client(FakePost(answer)).issue(capability_request())

    assert refused.value.code is RefusalCode.UNAVAILABLE
    assert "unknown" in str(refused.value)
    assert "nothing" not in str(refused.value)


def test_a_token_that_cannot_be_fetched_is_unavailable_and_nothing_is_sent() -> None:
    post = FakePost(FakeAnswer(200, ISSUED_BODY))
    token = FakeToken(error=RuntimeError("token-detail-7f3a"))

    with pytest.raises(SignerRefused) as refused:
        client(post, token).issue(capability_request())

    assert refused.value.code is RefusalCode.UNAVAILABLE
    assert "7f3a" not in str(refused.value)
    assert post.calls == []


# Logging


def test_a_failed_call_logs_only_the_exception_type(
    caplog: pytest.LogCaptureFixture,
) -> None:
    error = requests.ConnectionError(
        f"failed to reach {ORIGIN}{CAPABILITY_PATH}?sig=7f3a"
    )

    with caplog.at_level(logging.WARNING, logger="mao_cloud.signer_client"):
        with pytest.raises(SignerRefused):
            client(FakePost(error=error)).issue(capability_request())

    assert "ConnectionError" in caplog.text
    assert "7f3a" not in caplog.text
    assert TOKEN not in caplog.text


# Configuration


@pytest.mark.parametrize(
    "origin",
    [
        "",
        "signer.example.run.app",
        "http://signer.example.run.app",
        "https://signer.example.run.app/",
        "https://signer.example.run.app:443",
        "https://Signer.example.run.app",
    ],
)
def test_a_client_refuses_an_origin_that_is_not_an_https_origin(origin: str) -> None:
    with pytest.raises(ValueError, match="https origin"):
        HttpSignerClient(origin=origin, post=FakePost(), fetch_token=FakeToken())


@pytest.mark.parametrize("failure", ["timeout", "decode"])
def test_unavailable_can_follow_successful_remote_signing(failure: str) -> None:
    remotely_issued: list[IssuedCapability] = []

    def post(url: str, **kwargs: Any) -> FakeAnswer:
        remotely_issued.append(ISSUED)
        if failure == "timeout":
            raise requests.Timeout("response lost after signing")
        return FakeAnswer(200, json_error=True)

    signer = HttpSignerClient(origin=ORIGIN, post=post, fetch_token=FakeToken())

    with pytest.raises(SignerRefused) as refused:
        signer.issue(capability_request())

    assert remotely_issued == [ISSUED]
    assert refused.value.code is RefusalCode.UNAVAILABLE
    assert str(refused.value) == "the signer is unavailable; signing outcome is unknown"
