"""The CLI's caller token: minted from IAM for the invoker, sent on both headers, never printed.

The session is a fake that records the request. Nothing here reaches Google.
"""

from typing import Any

import pytest

from auth_support import (
    AUDIENCE,
    EMAIL_OWNER_A,
    UNKNOWN_RUN_ID,
    make_auth_world,
    mint,
)
from mao_cloud.cli import TokenError, invoker_headers, mint_invoker_token

IAM_URL = (
    "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/"
    f"{EMAIL_OWNER_A}:generateIdToken"
)


class FakeResponse:
    def __init__(self, status_code: int = 200, body: Any = None, *, raw: bytes | None = None):
        self.status_code = status_code
        self._body = body
        self._raw = raw

    def json(self) -> Any:
        if self._raw is not None:
            raise ValueError("not json")
        return self._body


class FakeSession:
    def __init__(self, response: FakeResponse) -> None:
        self.response = response
        self.calls: list[tuple[str, dict[str, Any]]] = []

    def post(self, url: str, **kwargs: Any) -> FakeResponse:
        self.calls.append((url, kwargs))
        return self.response


def test_token_is_minted_for_the_invoker_with_its_email_claim_and_a_timeout() -> None:
    token = mint()
    session = FakeSession(FakeResponse(200, {"token": token}))

    minted = mint_invoker_token(audience=AUDIENCE, invoker=EMAIL_OWNER_A, session=session)

    assert minted == token
    assert session.calls == [
        (
            IAM_URL,
            {"json": {"audience": AUDIENCE, "includeEmail": True}, "timeout": 30},
        )
    ]


@pytest.mark.parametrize(
    "audience",
    [
        "http://mao-cloud.example.test",
        "https://mao-cloud.example.test/",
        "https://mao-cloud.example.test/v1",
        "https://MAO-cloud.example.test",
        "mao-cloud.example.test",
    ],
)
def test_audience_outside_the_control_api_origin_is_refused_before_any_call(audience: str) -> None:
    session = FakeSession(FakeResponse(200, {"token": mint()}))

    with pytest.raises(TokenError):
        mint_invoker_token(audience=audience, invoker=EMAIL_OWNER_A, session=session)

    assert session.calls == []


@pytest.mark.parametrize(
    "invoker",
    [
        "owner@example.com",
        "Invoker-a@mao-test.iam.gserviceaccount.com",
        "invoker-a@mao-test.iam.gserviceaccount.com.evil.test",
        f"{EMAIL_OWNER_A}\n",
        "invoker-a@mao-test.iam.gserviceaccount.com/../scheduler",
    ],
)
def test_invoker_that_is_not_a_service_account_email_is_refused_before_any_call(
    invoker: str,
) -> None:
    session = FakeSession(FakeResponse(200, {"token": mint()}))

    with pytest.raises(TokenError):
        mint_invoker_token(audience=AUDIENCE, invoker=invoker, session=session)

    assert session.calls == []


def test_refusal_from_iam_names_the_status_and_never_the_response_body() -> None:
    denied = {"error": "caller lacks iam.serviceAccounts.getOpenIdToken"}
    session = FakeSession(FakeResponse(403, denied))

    with pytest.raises(TokenError) as refused:
        mint_invoker_token(audience=AUDIENCE, invoker=EMAIL_OWNER_A, session=session)

    assert str(refused.value) == "generateIdToken refused with HTTP 403"
    assert "getOpenIdToken" not in str(refused.value)


@pytest.mark.parametrize(
    "response",
    [
        FakeResponse(200, {}),
        FakeResponse(200, ["token", mint()]),
        FakeResponse(200, {"token": "not-a-compact-jws"}),
        FakeResponse(200, {"token": 12345}),
    ],
    ids=["no token", "non-object body", "token not a compact JWS", "token not a string"],
)
def test_success_without_a_usable_token_is_refused(response: FakeResponse) -> None:
    session = FakeSession(response)

    with pytest.raises(TokenError) as refused:
        mint_invoker_token(audience=AUDIENCE, invoker=EMAIL_OWNER_A, session=session)

    assert "generateIdToken returned no ID token" in str(refused.value)


def test_unreadable_success_body_is_refused_without_leaking_it() -> None:
    session = FakeSession(FakeResponse(200, raw=b"<html>upstream proxy page</html>"))

    with pytest.raises(TokenError) as refused:
        mint_invoker_token(audience=AUDIENCE, invoker=EMAIL_OWNER_A, session=session)

    assert "upstream proxy" not in str(refused.value)
    assert isinstance(refused.value.__cause__, ValueError)


def test_invoker_headers_carry_the_same_token_on_both_headers() -> None:
    token = mint()

    headers = invoker_headers(token)

    assert headers == {
        "Authorization": f"Bearer {token}",
        "X-Serverless-Authorization": f"Bearer {token}",
    }


@pytest.mark.parametrize(
    "token", ["not-a-token", "a.b", "a.b.c.d", "a.b.c=", "Bearer a.b.c"]
)
def test_invoker_headers_refuse_anything_that_is_not_a_compact_jws(token: str) -> None:
    with pytest.raises(TokenError):
        invoker_headers(token)


def test_token_minted_by_the_cli_authenticates_at_the_broker() -> None:
    # The round trip: the CLI's own minting and header code, then the production broker.
    token = mint_invoker_token(
        audience=AUDIENCE,
        invoker=EMAIL_OWNER_A,
        session=FakeSession(FakeResponse(200, {"token": mint()})),
    )
    world = make_auth_world()

    response = world.client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=invoker_headers(token))

    # Authenticated as owner A: an unknown run is a 404, not the 403 an unauthenticated caller gets.
    assert response.status_code == 404
    assert response.json()["error"]["code"] == "not_found"
