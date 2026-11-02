"""The controller-to-signer HTTP contract (DPES-899). Internal to Motif; not a protocol-v1 object.

Both ends import this module, so the request, answer and refusal shapes are defined once:

  POST {origin}/v1/capabilities
  Authorization: Bearer <Google ID token whose audience is the origin>
  body: one CapabilityRequest as JSON

  200 body: object_name, method, url, headers, expires_at (an aware ISO 8601 time)
  any other answer: {"refusal": <RefusalCode value>, "message": <that code's fixed message>}

A refusal names a code and its fixed message only. It never carries run data or the request.
"""

import re
from datetime import datetime
from typing import Any

from mao_cloud.signer import (
    CapabilityRequest,
    IssuedCapability,
    RefusalCode,
    SignerRefused,
)

CAPABILITY_PATH = "/v1/capabilities"

# The https origin of a Cloud Run service, with no path. It is also the ID-token audience.
SIGNER_ORIGIN = re.compile(r"https://[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?")

_ISSUED_FIELDS = frozenset({"object_name", "method", "url", "headers", "expires_at"})
_STATUS: dict[RefusalCode, int] = {
    RefusalCode.MALFORMED_REQUEST: 400,
    RefusalCode.UNAUTHENTICATED: 401,
    RefusalCode.UNAVAILABLE: 503,
}


def status_for(code: RefusalCode) -> int:
    return _STATUS.get(code, 403)


def request_body(request: CapabilityRequest) -> dict[str, Any]:
    return request.model_dump(mode="json")


def refusal_body(code: RefusalCode) -> dict[str, str]:
    return {"refusal": code.value, "message": str(SignerRefused(code))}


def issued_body(issued: IssuedCapability) -> dict[str, Any]:
    return {
        "object_name": issued.object_name,
        "method": issued.method,
        "url": issued.url,
        "headers": dict(issued.headers),
        "expires_at": issued.expires_at.isoformat(),
    }


def issued_from(body: object) -> IssuedCapability:
    """The one reading of a 200 answer. Any other shape raises ValueError."""
    if not isinstance(body, dict) or set(body) != _ISSUED_FIELDS:
        raise ValueError("the signer answer has an unexpected shape")
    headers = body["headers"]
    if not isinstance(headers, dict) or not all(
        isinstance(name, str) and isinstance(value, str)
        for name, value in headers.items()
    ):
        raise ValueError("the signer answer has unexpected headers")
    for field in ("object_name", "method", "url"):
        if not isinstance(body[field], str):
            raise ValueError(f"the signer answer has an unexpected {field}")
    expires = body["expires_at"]
    if not isinstance(expires, str):
        raise ValueError("the signer answer has an unexpected expiry")
    expires_at: datetime = datetime.fromisoformat(expires)
    if expires_at.tzinfo is None:
        raise ValueError("the signer answer expiry is not timezone-aware")
    return IssuedCapability(
        object_name=body["object_name"],
        method=body["method"],
        url=body["url"],
        headers=dict(headers),
        expires_at=expires_at,
    )


def refusal_code_from(body: object) -> RefusalCode | None:
    """The refusal code a non-200 answer names, or None when the answer names no known code."""
    if not isinstance(body, dict):
        return None
    try:
        return RefusalCode(body.get("refusal"))
    except (ValueError, TypeError):
        return None
