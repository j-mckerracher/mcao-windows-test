"""The controller's production signer client (DPES-899): one authenticated HTTPS call per capability.

The controller holds no signing method. This client asks the signer service at its configured
origin for one capability, presenting a Google ID token whose audience is that origin. It returns the
capability the signer issued, or raises SignerRefused. It never builds a signed URL itself.

A refusal the signer names is passed through as its code. A token that cannot be fetched sends no
request, so nothing is signed; it raises SignerRefused(UNAVAILABLE) with the same fixed message as
every other unavailable outcome. Every other failure after a request was sent, such as a connection
that fails or times out, or an answer of the wrong shape, also raises SignerRefused(UNAVAILABLE),
and the signing outcome is unknown. A timeout or unusable answer can follow successful signing. Only
an exception type is logged, so no token, URL or run data reaches a log. Redirects are not followed,
so the bearer token is never sent to a host other than the configured origin.

The signer's behaviour is exercised here with an injected post and token source. Live calls to a
deployed signer are unverified until a deployed run proves them.
"""

import logging
from collections.abc import Callable
from typing import Any

import requests
from google.auth.transport import requests as google_requests
from google.oauth2 import id_token

from mao_cloud.signer import (
    CapabilityRequest,
    IssuedCapability,
    RefusalCode,
    SignerRefused,
)
from mao_cloud.signer_wire import (
    CAPABILITY_PATH,
    SIGNER_ORIGIN,
    issued_from,
    refusal_code_from,
    request_body,
)

TIMEOUT_SECONDS = 10.0

_LOG = logging.getLogger(__name__)

Post = Callable[..., requests.Response]
FetchToken = Callable[[str], str]


def fetch_id_token(audience: str) -> str:
    """A Google ID token for the audience, fetched through the runtime's ambient identity."""
    return id_token.fetch_id_token(google_requests.Request(), audience)


class HttpSignerClient:
    def __init__(
        self,
        *,
        origin: str,
        post: Post = requests.post,
        fetch_token: FetchToken = fetch_id_token,
        timeout_seconds: float = TIMEOUT_SECONDS,
    ) -> None:
        # The origin is also the token audience and the only host the bearer token may reach.
        if SIGNER_ORIGIN.fullmatch(origin) is None:
            raise ValueError("the signer origin must be an https origin with no path")
        self._origin = origin
        self._url = origin + CAPABILITY_PATH
        self._post = post
        self._fetch_token = fetch_token
        self._timeout = timeout_seconds

    def issue(self, request: CapabilityRequest) -> IssuedCapability:
        try:
            token = self._fetch_token(self._origin)
        except Exception as exc:
            _LOG.warning(
                "signer call not made: no identity token (%s)", type(exc).__name__
            )
            raise SignerRefused(RefusalCode.UNAVAILABLE) from None
        try:
            answer = self._post(
                self._url,
                json=request_body(request),
                headers={
                    "Authorization": f"Bearer {token}",
                    "X-Serverless-Authorization": f"Bearer {token}",
                },
                timeout=self._timeout,
                allow_redirects=False,
            )
        except requests.RequestException as exc:
            _LOG.warning("signer call failed (%s)", type(exc).__name__)
            raise SignerRefused(RefusalCode.UNAVAILABLE) from None
        body: Any
        try:
            body = answer.json()
        except ValueError:
            body = None
        if answer.status_code == 200:
            try:
                return issued_from(body)
            except ValueError:
                _LOG.warning("signer answer had an unexpected shape")
                raise SignerRefused(RefusalCode.UNAVAILABLE) from None
        code = refusal_code_from(body)
        if code is None:
            _LOG.warning(
                "signer answer named no known refusal (status %s)", answer.status_code
            )
            raise SignerRefused(RefusalCode.UNAVAILABLE)
        raise SignerRefused(code)
