"""The bearer credential and the bounded certificate fetch that every Google ID-token check shares.

The broker and the signer verify the same kind of caller token. This module owns the one reading of
the Authorization header and the one bounded certificate fetch, so the two services cannot drift.
"""

import re
from collections.abc import Callable
from typing import Any

from fastapi import Request

# Google's certificate endpoint answers in well under a second. A slower fetch refuses the caller
# rather than holding a worker thread for as long as the endpoint stalls.
CERT_FETCH_TIMEOUT_SECONDS = 5.0

# A compact JWS is three base64url segments and nothing else. Anything else is not a credential.
_BEARER_CREDENTIAL = re.compile(r"^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$")


def bounded_fetch(
    transport: Callable[..., Any], timeout_seconds: float
) -> Callable[..., Any]:
    """Pin each certificate fetch to one timeout. The transport must honor the timeout it is given."""

    def fetch(url: str, method: str = "GET", **kwargs: Any) -> Any:
        return transport(url, method=method, **{**kwargs, "timeout": timeout_seconds})

    return fetch


def bearer_credential(request: Request) -> str | None:
    """The one bearer credential in Authorization. Ambiguous or malformed input is no credential."""
    values = request.headers.getlist("authorization")
    if len(values) != 1:
        return None
    scheme, _, credential = values[0].partition(" ")
    if scheme.lower() != "bearer" or not _BEARER_CREDENTIAL.fullmatch(credential):
        return None
    return credential
