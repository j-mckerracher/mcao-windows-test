"""Caller identity: a Google ID token resolves to one fixed identity, or to nothing.

These tests call the production BearerIdentity adapter with real Starlette requests, so every
refusal below is the adapter's own decision rather than a route's. Refusals are identical in
outcome; the audit log names the reason category and never the token.
"""

import logging
from collections.abc import Callable
from typing import Any

import pytest
from starlette.requests import Request

from auth_support import (
    AUDIENCE,
    EMAIL_OPERATOR,
    EMAIL_OWNER_A,
    EMAIL_OWNER_B,
    EMAIL_SCHEDULER,
    GOOGLE_SIGNER,
    ROGUE_SIGNER,
    SUBJECT_OPERATOR,
    SUBJECT_OWNER_A,
    SUBJECT_OWNER_B,
    SUBJECT_SCHEDULER,
    UNPUBLISHED_SIGNER,
    CertTransport,
    MutableIdentities,
    mint,
)
from fakes import OWNER_A, OWNER_B
from mao_cloud.broker import BearerIdentity


def bearer_adapter(
    identities: MutableIdentities | None = None, transport: CertTransport | None = None
) -> BearerIdentity:
    return BearerIdentity(
        audience=AUDIENCE,
        directory=identities or MutableIdentities(),
        transport=transport or CertTransport(GOOGLE_SIGNER),
    )


def request_with(*headers: tuple[str, str]) -> Request:
    raw = [(name.lower().encode("latin-1"), value.encode("latin-1")) for name, value in headers]
    return Request(
        {
            "type": "http",
            "method": "GET",
            "scheme": "https",
            "server": ("mao-cloud.example.test", 443),
            "path": "/v1/runs/run_example",
            "root_path": "",
            "query_string": b"",
            "headers": raw,
        }
    )


def bearer_header(token: str) -> tuple[str, str]:
    return ("Authorization", f"Bearer {token}")


def test_owner_token_resolves_to_the_fixed_owner_for_its_subject() -> None:
    adapter = bearer_adapter()
    request = request_with(bearer_header(mint()))

    assert adapter.owner_of(request) == OWNER_A
    assert adapter.is_controller(request) is False


def test_second_owner_subject_resolves_to_its_own_owner() -> None:
    adapter = bearer_adapter()
    token = mint(subject=SUBJECT_OWNER_B, email=EMAIL_OWNER_B)

    assert adapter.owner_of(request_with(bearer_header(token))) == OWNER_B


def test_operator_is_a_controller_and_never_an_owner() -> None:
    adapter = bearer_adapter()
    request = request_with(bearer_header(mint(subject=SUBJECT_OPERATOR, email=EMAIL_OPERATOR)))

    assert adapter.owner_of(request) is None
    assert adapter.is_controller(request) is True


def test_scheduler_is_a_controller_and_never_an_owner() -> None:
    adapter = bearer_adapter()
    request = request_with(bearer_header(mint(subject=SUBJECT_SCHEDULER, email=EMAIL_SCHEDULER)))

    assert adapter.owner_of(request) is None
    assert adapter.is_controller(request) is True


REFUSED_TOKENS: dict[str, Callable[[], str]] = {
    "wrong audience": lambda: mint(audience="https://elsewhere.example.test"),
    "wrong issuer": lambda: mint(issuer="https://issuer.example.test"),
    "forged signature under a trusted key id": lambda: mint(ROGUE_SIGNER),
    "signing key not in the published set": lambda: mint(UNPUBLISHED_SIGNER),
    "expired": lambda: mint(lifetime_seconds=-60),
    "subject not in the identity map": lambda: mint(subject="999999999999999999"),
    "email differs from the subject's service account": lambda: mint(email=EMAIL_OWNER_B),
    "email not verified": lambda: mint(email_verified=False),
    "email claim absent": lambda: mint(email=None),
}


@pytest.mark.parametrize("make_token", REFUSED_TOKENS.values(), ids=list(REFUSED_TOKENS))
def test_token_failing_verification_or_mapping_resolves_to_nothing(
    make_token: Callable[[], str],
) -> None:
    adapter = bearer_adapter()
    request = request_with(bearer_header(make_token()))

    assert adapter.owner_of(request) is None
    assert adapter.is_controller(request) is False


MALFORMED_HEADERS: dict[str, Callable[[], tuple[tuple[str, str], ...]]] = {
    "no authorization header": lambda: (),
    "basic scheme": lambda: (("Authorization", "Basic dXNlcjpwYXNz"),),
    "bearer with no token": lambda: (("Authorization", "Bearer "),),
    "two segments": lambda: (("Authorization", "Bearer abc.def"),),
    "four segments": lambda: (("Authorization", "Bearer a.b.c.d"),),
    "three segments that are not a token": lambda: (("Authorization", "Bearer a.b.c"),),
    "undecodable payload": lambda: (
        ("Authorization", "Bearer eyJhbGciOiJSUzI1NiJ9.bm90IGpzb24.AAAA"),
    ),
    "header with embedded whitespace": lambda: (
        ("Authorization", f"Bearer {mint()} extra"),
    ),
    "two authorization headers both valid": lambda: (
        bearer_header(mint()),
        bearer_header(mint()),
    ),
}


@pytest.mark.parametrize("make_headers", MALFORMED_HEADERS.values(), ids=list(MALFORMED_HEADERS))
def test_malformed_or_ambiguous_authorization_resolves_to_nothing(
    make_headers: Callable[[], tuple[tuple[str, str], ...]],
) -> None:
    adapter = bearer_adapter()
    request = request_with(*make_headers())

    assert adapter.owner_of(request) is None
    assert adapter.is_controller(request) is False


def test_serverless_header_alone_is_not_the_application_identity() -> None:
    adapter = bearer_adapter()
    request = request_with(("X-Serverless-Authorization", f"Bearer {mint()}"))

    assert adapter.owner_of(request) is None


def test_owner_header_from_the_client_is_ignored_for_an_authenticated_owner() -> None:
    adapter = bearer_adapter()
    request = request_with(bearer_header(mint()), ("x-owner", OWNER_B))

    assert adapter.owner_of(request) == OWNER_A


def test_certificate_fetch_failure_refuses_rather_than_trusting_the_token() -> None:
    transport = CertTransport(GOOGLE_SIGNER)
    transport.status = 503
    adapter = bearer_adapter(transport=transport)

    assert adapter.owner_of(request_with(bearer_header(mint()))) is None
    assert transport.fetches == 1


class UnreachableTransport:
    """A certificate fetch that fails at the network layer rather than with an HTTP status."""

    def __call__(self, url: str, method: str = "GET", **_: Any) -> None:
        raise ConnectionError("certificate endpoint unreachable")


def test_unreachable_certificate_endpoint_refuses_instead_of_erroring(
    caplog: pytest.LogCaptureFixture,
) -> None:
    caplog.set_level(logging.INFO, logger="mao_cloud.audit")
    adapter = BearerIdentity(
        audience=AUDIENCE,
        directory=MutableIdentities(),
        transport=UnreachableTransport(),
    )

    assert adapter.owner_of(request_with(bearer_header(mint()))) is None
    assert "verifier_unavailable" in caplog.text
    assert "ConnectionError" in caplog.text


@pytest.mark.parametrize("make_token", REFUSED_TOKENS.values(), ids=list(REFUSED_TOKENS))
def test_refusals_are_audited_without_the_token_or_its_signature(
    caplog: pytest.LogCaptureFixture, make_token: Callable[[], str]
) -> None:
    caplog.set_level(logging.INFO, logger="mao_cloud.audit")
    token = make_token()
    adapter = bearer_adapter()

    adapter.owner_of(request_with(bearer_header(token)))

    assert any(record.name == "mao_cloud.audit" for record in caplog.records)
    assert token not in caplog.text
    assert token.rsplit(".", 1)[1] not in caplog.text


def test_accepted_caller_is_audited_by_subject_and_role_without_the_token(
    caplog: pytest.LogCaptureFixture,
) -> None:
    caplog.set_level(logging.INFO, logger="mao_cloud.audit")
    token = mint()
    adapter = bearer_adapter()

    adapter.owner_of(request_with(bearer_header(token)))

    audit = [record.getMessage() for record in caplog.records if record.name == "mao_cloud.audit"]
    assert audit, "an accepted owner must leave an audit record"
    assert any(SUBJECT_OWNER_A in line and "owner" in line for line in audit)
    assert token not in caplog.text
    assert EMAIL_OWNER_A not in caplog.text


def test_rejected_subject_is_audited_as_unknown_not_bound_to_any_owner(
    caplog: pytest.LogCaptureFixture,
) -> None:
    caplog.set_level(logging.INFO, logger="mao_cloud.audit")
    adapter = bearer_adapter()

    adapter.owner_of(request_with(bearer_header(mint(subject="999999999999999999"))))

    audit = [record.getMessage() for record in caplog.records if record.name == "mao_cloud.audit"]
    assert any("unknown_subject" in line for line in audit)
    assert not any(OWNER_A in line or OWNER_B in line for line in audit)
